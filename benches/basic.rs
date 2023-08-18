use std::sync::Arc;

use criterion::*;

static KB: usize = 1024;

fn try_broadcast(c: &mut Criterion) {
    let mut group = c.benchmark_group("try_broadcast");
    for size in [64, 256, KB, 4 * KB, 16 * KB, 64 * KB, 256 * KB].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("try_broadcast hyperbus", size),
            size,
            |b, &size| {
                b.iter_batched(
                    || hyperbus::Bus::<u32>::new(size),
                    |mut tx| {
                        for i in 0..(size - 1) {
                            black_box(tx.try_broadcast(black_box(i as u32))).unwrap();
                        }
                    },
                    BatchSize::SmallInput,
                )
            },
        );

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("try_send crossbeam", size),
            size,
            |b, &size| {
                b.iter_batched(
                    || crossbeam_channel::bounded::<u32>(size),
                    |(tx, _rx)| {
                        for i in 0..(size - 1) {
                            black_box(tx.try_send(black_box(i as u32))).unwrap();
                        }
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
    group.finish();
}

fn try_broadcast_try_recv(c: &mut Criterion) {
    let mut group = c.benchmark_group("try_broadcast + try_recv");
    for size in [64, 256, KB, 4 * KB, 16 * KB, 64 * KB, 256 * KB].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("try_broadcast + try_recv asyncbus", size),
            size,
            |b, &size| {
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
            },
        );

        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("try_send + try_recv crossbeam", size),
            size,
            |b, &size| {
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
            },
        );
    }
    group.finish();
}

fn broadcast_recv_numbered(c: &mut Criterion) {
    let mut group = c.benchmark_group("broadcast_multi_thread_512_elements");
    let num_elements = 1024 * 1024;
    for threads in [1, 2, 4, 6, 8, 12, 16].iter() {
        group.throughput(Throughput::Elements(num_elements));
        group.bench_with_input(
            BenchmarkId::new("try_broadcast + try_recv asyncbus", threads),
            threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        let mut bus = hyperbus::Bus::<u32>::new(512);
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

        /*
        group.throughput(Throughput::Elements(*threads as u64));
        group.bench_with_input(
            BenchmarkId::new("try_broadcast + try_recv asyncbus", threads),
            threads,
            |b, &threads| {
                b.iter_batched(
                    || {
                        let rxs = (0..threads).map(|_| {
                            let rx = bus.add_rx();
                            std::thread::spawn(move || {
                                futures::executor::block_on(async move {
                                    for i in 0..num_elements {
                                        assert_eq!(i as u32, rx.recv().await.unwrap());
                                    }
                                });
                            })
                        });
                        (bus, rxs)
                    },
                    |(mut tx, mut rx)| {},
                    BatchSize::PerIteration,
                )
            },
        );
        */
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
