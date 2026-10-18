#!/usr/bin/perl
# A long-running filter process for tests, written for girt.
#
# Speaks protocol version 2 with the clean and smudge capabilities. Clean upper-cases content and
# smudge lower-cases it. A path starting with `error` gets `status=error`, `abort` gets
# `status=abort`, and `late-error` sends content followed by a trailing `status=error`. Each
# request's header lines are appended to the file named by the first argument.
use strict;
use warnings;

binmode STDIN;
binmode STDOUT;
$| = 1;
my $log = shift @ARGV;

sub read_packet {
    my $header;
    my $got = read(STDIN, $header, 4);
    return undef unless defined $got && $got == 4;
    my $length = hex($header);
    return "" if $length == 0;
    my $data = "";
    while (length($data) < $length - 4) {
        my $chunk;
        read(STDIN, $chunk, $length - 4 - length($data)) or die "short read";
        $data .= $chunk;
    }
    return $data;
}

sub read_list {
    my @lines;
    while (1) {
        my $packet = read_packet();
        return undef unless defined $packet;
        last if $packet eq "";
        $packet =~ s/\n$//;
        push @lines, $packet;
    }
    return \@lines;
}

sub write_packet { my ($data) = @_; printf STDOUT "%04x%s", length($data) + 4, $data; }
sub write_text { write_packet("$_[0]\n"); }
sub flush_packet { print STDOUT "0000"; }

my $hello = read_list() or exit 0;
die "bad hello" unless $hello->[0] eq "git-filter-client";
write_text("git-filter-server");
write_text("version=2");
flush_packet();
read_list();
write_text("capability=clean");
write_text("capability=smudge");
flush_packet();

open(my $out, ">>", $log) or die "log: $!";
while (my $header = read_list()) {
    my %keys = map { split(/=/, $_, 2) } @$header;
    my $content = "";
    while (1) {
        my $packet = read_packet();
        die "eof in content" unless defined $packet;
        last if $packet eq "";
        $content .= $packet;
    }
    print $out join("|", @$header), "\n";
    my $path = $keys{pathname};
    if ($path =~ /^error/) {
        write_text("status=error");
        flush_packet();
        next;
    }
    if ($path =~ /^abort/) {
        write_text("status=abort");
        flush_packet();
        next;
    }
    my $result = $keys{command} eq "clean" ? uc($content) : lc($content);
    write_text("status=success");
    flush_packet();
    for (my $at = 0; $at < length($result); $at += 65516) {
        write_packet(substr($result, $at, 65516));
    }
    flush_packet();
    if ($path =~ /^late-error/) {
        write_text("status=error");
    }
    flush_packet();
}
close($out);
