//! Ordered replacement reads with one pending payload and a private base cache per worker.
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

use super::{Error, FilePack, Object, PrivateCache, ReadLimits, decode, window};

impl FilePack {
    /// Feeds checked payloads to one consumer in exactly `positions` order. Dropping the
    /// consumer closes every rendezvous channel before joining, including on errors or panic.
    pub(crate) fn with_ordered_reads<T>(
        &self,
        positions: &[usize],
        limits: ReadLimits,
        max_threads: usize,
        cancel: &AtomicBool,
        consume: impl FnOnce(&mut dyn Iterator<Item = Result<Object, Error>>) -> T,
    ) -> T {
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(max_threads)
            .min(positions.len())
            .max(1);
        if threads == 1 {
            return consume(
                &mut positions
                    .iter()
                    .map(|&position| self.read(position, limits, cancel)),
            );
        }
        let cache_limit = super::super::reader::BaseCache::LIMIT / threads;
        std::thread::scope(|scope| {
            let mut receivers = Vec::with_capacity(threads);
            for worker in 0..threads {
                let (send, receive) = mpsc::sync_channel(0);
                receivers.push(receive);
                scope.spawn(move || {
                    let _headers = window::HeaderScope::new();
                    let _ordered = window::HeaderScope::ordered();
                    let source = PrivateCache {
                        pack: self,
                        cache: super::super::reader::BaseCache::with_limit(cache_limit),
                    };
                    // Interleave neighbouring positions: every worker retains offset locality
                    // while all workers can decode the next group ahead of the serial parser.
                    for &position in positions.iter().skip(worker).step_by(threads) {
                        let result = decode(&source, position, limits, cancel);
                        let failed = result.is_err();
                        if send.send(result).is_err() || failed {
                            return;
                        }
                    }
                });
            }
            let mut reads = (0..positions.len()).map(|index| {
                receivers
                    .get(index % threads)
                    .and_then(|receiver| receiver.recv().ok())
                    .unwrap_or(Err(Error::Corrupt("ordered read worker stopped")))
            });
            consume(&mut reads)
            // `reads` and its receivers drop before the scope joins its workers.
        })
    }
}
