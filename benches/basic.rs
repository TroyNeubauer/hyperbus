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

criterion_group!(benches, try_broadcast, try_broadcast_try_recv,);

criterion_main!(benches);
