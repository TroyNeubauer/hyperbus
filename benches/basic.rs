use std::sync::Arc;

use criterion::*;

static KB: usize = 1024;

fn try_broadcast(c: &mut Criterion) {
    let mut group = c.benchmark_group("try_broadcast");
    for size in [64, 256, KB, 4 * KB, 16 * KB, 64 * KB].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("hyperbus", size), size, |b, &size| {
            b.iter_batched(
                || hyperbus::Bus::<u32>::new(size),
                |mut tx| {
                    for i in 0..(size - 1) {
                        black_box(tx.try_broadcast(black_box(i as u32))).unwrap();
                    }
                },
                BatchSize::SmallInput,
            )
        });

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("crossbeam", size), size, |b, &size| {
            b.iter_batched(
                || crossbeam_channel::bounded::<u32>(size),
                |(tx, _rx)| {
                    for i in 0..(size - 1) {
                        black_box(tx.try_send(black_box(i as u32))).unwrap();
                    }
                },
                BatchSize::SmallInput,
            )
        });

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("tokio", size), size, |b, &size| {
            b.iter_batched(
                || tokio::sync::broadcast::channel::<u32>(size),
                |(tx, _rx)| {
                    for i in 0..(size - 1) {
                        black_box(tx.send(black_box(i as u32))).unwrap();
                    }
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn try_broadcast_try_recv(c: &mut Criterion) {
    let mut group = c.benchmark_group("try_broadcast + try_recv");
    for size in [64, 256, KB, 4 * KB, 16 * KB, 64 * KB].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("hyperbus", size), size, |b, &size| {
            b.iter_batched(
                || {
                    let mut bus = hyperbus::Bus::<u32>::new(size);
                    let rx = bus.add_rx();
                    (bus, rx)
                },
                |(mut tx, mut rx)| {
                    for i in 0..(size - 1) {
                        black_box(tx.try_broadcast(black_box(i as u32))).unwrap();
                    }
                    for i in 0..(size - 1) {
                        assert_eq!(i as u32, rx.try_recv().unwrap());
                    }
                },
                BatchSize::SmallInput,
            )
        });

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::new("crossbeam", size), size, |b, &size| {
            b.iter_batched(
                || crossbeam_channel::bounded::<u32>(size),
                |(tx, rx)| {
                    for i in 0..(size - 1) {
                        black_box(tx.try_send(black_box(i as u32))).unwrap();
                    }
                    for i in 0..(size - 1) {
                        assert_eq!(i as u32, rx.try_recv().unwrap());
                    }
                },
                BatchSize::SmallInput,
            )
        });

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("tokio broadcast", size),
            size,
            |b, &size| {
                b.iter_batched(
                    || tokio::sync::broadcast::channel::<u32>(size),
                    |(tx, mut rx)| {
                        for i in 0..(size - 1) {
                            black_box(tx.send(black_box(i as u32))).unwrap();
                        }
                        for i in 0..(size - 1) {
                            assert_eq!(i as u32, rx.try_recv().unwrap());
                        }
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
    group.finish();
}

fn broadcast_recv_numbered(c: &mut Criterion) {
    let mut group = c.benchmark_group("broadcast_multi_thread_16k_elements");
    let num_elements = 16 * 1024;
    let buffer_size = 512;
    for threads in [1, 2, 4, 6, 8, 12, 16].iter() {
        group.throughput(Throughput::Elements(num_elements));
        group.bench_with_input(
            BenchmarkId::new("hyperbus", threads),
            threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        let mut bus = hyperbus::Bus::<u32>::new(buffer_size);
                        let barrier = Arc::new(tokio::sync::Barrier::new(threads + 1));
                        let rxs: Vec<_> = (0..threads)
                            .map(|_| {
                                let mut rx = bus.add_rx();
                                let barrier = Arc::clone(&barrier);
                                std::thread::spawn(move || {
                                    futures::executor::block_on(async move {
                                        barrier.wait().await;
                                        for i in 0..num_elements {
                                            assert_eq!(i as u32, rx.recv().await.unwrap());
                                        }
                                    });
                                })
                            })
                            .collect();

                        // Unleash the hounds
                        futures::executor::block_on(barrier.wait());
                        (bus, rxs)
                    },
                    |(mut bus, rxs)| {
                        futures::executor::block_on(async move {
                            for i in 0..num_elements {
                                bus.broadcast(i as u32).await;
                            }
                        });
                        rxs.into_iter().for_each(|t| t.join().unwrap());
                    },
                    BatchSize::PerIteration,
                )
            },
        );

        group.throughput(Throughput::Elements(num_elements));
        group.bench_with_input(
            BenchmarkId::new("tokio", threads),
            threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        let (tx, rx) = tokio::sync::broadcast::channel::<u32>(buffer_size);
                        let barrier = Arc::new(tokio::sync::Barrier::new(threads + 1));
                        let rxs: Vec<_> = (0..threads)
                            .map(|_| {
                                let mut rx = tx.subscribe();
                                let barrier = Arc::clone(&barrier);
                                std::thread::spawn(move || {
                                    futures::executor::block_on(async move {
                                        barrier.wait().await;
                                        for i in 0..num_elements {
                                            assert_eq!(i as u32, rx.recv().await.unwrap());
                                        }
                                    });
                                })
                            })
                            .collect();

                        drop(rx);

                        // Unleash the hounds
                        futures::executor::block_on(barrier.wait());
                        (tx, rxs)
                    },
                    |(tx, rxs)| {
                        futures::executor::block_on(async move {
                            for i in 0..num_elements {
                                while tx.len() >= buffer_size - 4 {
                                    std::hint::spin_loop();
                                }
                                tx.send(i as u32).unwrap();
                            }
                        });
                        rxs.into_iter().for_each(|t| t.join().unwrap());
                    },
                    BatchSize::PerIteration,
                )
            },
        );

        group.throughput(Throughput::Elements(num_elements));
        group.bench_with_input(
            BenchmarkId::new("crossbeam", threads),
            threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        let barrier = Arc::new(std::sync::Barrier::new(threads + 1));
                        let threads_tx: Vec<_> = (0..threads)
                            .map(|_| {
                                // crossbeam doesn't have a broadcast MPMC channel, so fake one
                                // with a tx + rx for each thread, and have the send send one copy
                                // to each one
                                let (tx, rx) = crossbeam_channel::bounded::<u32>(buffer_size);
                                let barrier = Arc::clone(&barrier);
                                let thread = std::thread::spawn(move || {
                                    barrier.wait();
                                    for i in 0..num_elements {
                                        assert_eq!(i as u32, rx.recv().unwrap());
                                    }
                                });

                                (thread, tx)
                            })
                            .collect();

                        // Unleash the hounds
                        barrier.wait();
                        threads_tx
                    },
                    |threads_tx| {
                        for i in 0..num_elements {
                            for (_t, tx) in &threads_tx {
                                tx.send(i as u32).unwrap();
                            }
                        }
                        threads_tx
                            .into_iter()
                            .for_each(|(t, _tx)| t.join().unwrap());
                    },
                    BatchSize::PerIteration,
                )
            },
        );

        group.throughput(Throughput::Elements(num_elements));
        group.bench_with_input(
            BenchmarkId::new("multiqueue", threads),
            threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        let (tx, rx) = multiqueue::broadcast_queue(buffer_size as u64);
                        let barrier = Arc::new(std::sync::Barrier::new(threads + 1));
                        let rxs: Vec<_> = (0..threads)
                            .map(|_| {
                                let rx = rx.add_stream();
                                let barrier = Arc::clone(&barrier);
                                std::thread::spawn(move || {
                                    futures::executor::block_on(async move {
                                        barrier.wait();
                                        for i in 0..num_elements {
                                            assert_eq!(i as u32, rx.recv().unwrap());
                                        }
                                    });
                                })
                            })
                            .collect();

                        rx.unsubscribe();

                        // Unleash the hounds
                        barrier.wait();
                        (tx, rxs)
                    },
                    |(tx, rxs)| {
                        futures::executor::block_on(async move {
                            for i in 0..num_elements {
                                loop {
                                    // no blocking send method, so we must spin
                                    if tx.try_send(i as u32).is_ok() {
                                        break;
                                    }
                                    std::hint::spin_loop();
                                }
                            }
                        });
                        rxs.into_iter().for_each(|t| t.join().unwrap());
                    },
                    BatchSize::PerIteration,
                )
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    try_broadcast,
    try_broadcast_try_recv,
    broadcast_recv_numbered
);

criterion_main!(benches);

