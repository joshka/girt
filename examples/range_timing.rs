//! Time ahead/behind counts and range walks for explicit commit pairs.
//! Arguments: repository, rounds, then one or more `LEFT...RIGHT` pairs of full commit IDs.
//! Prints, per pair, the counts and the best round for `ahead_behind(LEFT, RIGHT)` and for
//! `walk_range([RIGHT], [LEFT])` (`git rev-list LEFT..RIGHT`). Compare against
//! `git rev-list --left-right --count LEFT...RIGHT` and `git rev-list --count LEFT..RIGHT`.
//! Uses trusted history limits, since long Linux ranges explore more than the default bound.
use std::hint::black_box;
use std::time::{Duration, Instant};

use girt::{HistoryLimits, ObjectId, PackLimits, Repository};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected repository path")?;
    let rounds: usize = args.next().ok_or("expected number of rounds")?.parse()?;
    let start = Instant::now();
    let repo = Repository::open(path)?;
    let objects = repo.objects_with(PackLimits::trusted())?;
    println!("open {:.1} ms", ms(start.elapsed()));
    for pair in args {
        let (left, right) = pair.split_once("...").ok_or("expected LEFT...RIGHT")?;
        let (left, right): (ObjectId, ObjectId) = (left.parse()?, right.parse()?);
        let mut counts = None;
        let mut best_counts = Duration::MAX;
        let mut walked = 0;
        let mut best_walk = Duration::MAX;
        for _ in 0..rounds {
            let start = Instant::now();
            counts = Some(black_box(objects.ahead_behind_with(
                left,
                right,
                HistoryLimits::trusted(),
            )?));
            best_counts = best_counts.min(start.elapsed());
            let start = Instant::now();
            walked =
                black_box(objects.walk_range_with(&[right], &[left], HistoryLimits::trusted())?)
                    .len();
            best_walk = best_walk.min(start.elapsed());
        }
        let counts = counts.ok_or("expected at least one round")?;
        println!(
            "{pair}: ahead {} behind {} in {:.2} ms; range {walked} in {:.2} ms",
            counts.ahead,
            counts.behind,
            ms(best_counts),
            ms(best_walk),
        );
    }
    Ok(())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}
