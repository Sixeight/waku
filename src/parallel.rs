use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn map<T: Sync, U: Send>(items: &[T], f: impl Fn(&T) -> U + Sync) -> Vec<U> {
    map_with_limit(items, 8, f)
}

pub(crate) fn map_with_limit<T: Sync, U: Send>(
    items: &[T],
    limit: usize,
    f: impl Fn(&T) -> U + Sync,
) -> Vec<U> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(limit.max(1))
        .min(items.len());
    if workers <= 1 {
        return items.iter().map(f).collect();
    }

    let next = AtomicUsize::new(0);
    let mut results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let f = &f;
                let next = &next;
                scope.spawn(move || {
                    let mut results = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break;
                        };
                        results.push((index, f(item)));
                    }
                    results
                })
            })
            .collect();
        let mut results = Vec::with_capacity(items.len());
        for handle in handles {
            match handle.join() {
                Ok(batch) => results.extend(batch),
                Err(error) => std::panic::resume_unwind(error),
            }
        }
        results
    });
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn preserves_order_and_processes_each_item_once_with_bounded_workers() {
        let items: Vec<_> = (0..1_000).collect();
        let seen = Mutex::new(Vec::new());
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let results = map(&items, |index| {
            let count = active.fetch_add(1, Ordering::Relaxed) + 1;
            peak.fetch_max(count, Ordering::Relaxed);
            seen.lock().unwrap().push(*index);
            std::thread::yield_now();
            active.fetch_sub(1, Ordering::Relaxed);
            index.to_string()
        });
        assert_eq!(
            results,
            items.iter().map(usize::to_string).collect::<Vec<_>>()
        );
        let mut seen = seen.into_inner().unwrap();
        seen.sort_unstable();
        assert_eq!(seen, items);
        assert!(peak.load(Ordering::Relaxed) <= 8);
    }

    #[test]
    fn single_worker_uses_the_calling_thread_and_preserves_order() {
        let caller = std::thread::current().id();
        let results = map_with_limit(&[3, 2, 1], 1, |item| {
            assert_eq!(std::thread::current().id(), caller);
            item * 2
        });
        assert_eq!(results, [6, 4, 2]);
    }

    #[test]
    fn empty_input_does_not_call_worker() {
        assert!(map::<(), ()>(&[], |_| panic!("unexpected worker")).is_empty());
    }
}
