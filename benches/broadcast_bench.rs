use async_broadcast::broadcast;
use criterion::{criterion_group, criterion_main, Criterion};
use futures_lite::future::block_on;
pub fn broadcast_and_recv(c: &mut Criterion) {
    let (s, r1) = broadcast(1);

    let mut n = 0;
    c.bench_function("1 -> 1", |b| {
        b.iter(|| {
            block_on(async {
                s.broadcast(n).await.unwrap();
                assert_eq!(r1.recv().await.unwrap(), n);
                n += 1;
            })
        })
    });

    let r2 = r1.clone();

    c.bench_function("1 -> 2", |b| {
        b.iter(|| {
            block_on(async {
                s.broadcast(n).await.unwrap();
                assert_eq!(r1.recv().await.unwrap(), n);
                assert_eq!(r2.recv().await.unwrap(), n);
                n += 1;
            })
        })
    });

    let r3 = r1.clone();
    let r4 = r1.clone();

    c.bench_function("1 -> 4", |b| {
        b.iter(|| {
            block_on(async {
                s.broadcast(n).await.unwrap();
                assert_eq!(r1.recv().await.unwrap(), n);
                assert_eq!(r2.recv().await.unwrap(), n);
                assert_eq!(r3.recv().await.unwrap(), n);
                assert_eq!(r4.recv().await.unwrap(), n);
                n += 1;
            })
        })
    });

    let r5 = r1.clone();
    let r6 = r1.clone();
    let r7 = r1.clone();
    let r8 = r1.clone();

    c.bench_function("1 -> 8", |b| {
        b.iter(|| {
            block_on(async {
                s.broadcast(n).await.unwrap();
                assert_eq!(r1.recv().await.unwrap(), n);
                assert_eq!(r2.recv().await.unwrap(), n);
                assert_eq!(r3.recv().await.unwrap(), n);
                assert_eq!(r4.recv().await.unwrap(), n);
                assert_eq!(r5.recv().await.unwrap(), n);
                assert_eq!(r6.recv().await.unwrap(), n);
                assert_eq!(r7.recv().await.unwrap(), n);
                assert_eq!(r8.recv().await.unwrap(), n);
                n += 1;
            })
        })
    });

    let r9 = r1.clone();
    let r10 = r1.clone();
    let r11 = r1.clone();
    let r12 = r1.clone();
    let r13 = r1.clone();
    let r14 = r1.clone();
    let r15 = r1.clone();
    let r16 = r1.clone();

    c.bench_function("1 -> 16", |b| {
        b.iter(|| {
            block_on(async {
                s.broadcast(n).await.unwrap();
                assert_eq!(r1.recv().await.unwrap(), n);
                assert_eq!(r2.recv().await.unwrap(), n);
                assert_eq!(r3.recv().await.unwrap(), n);
                assert_eq!(r4.recv().await.unwrap(), n);
                assert_eq!(r5.recv().await.unwrap(), n);
                assert_eq!(r6.recv().await.unwrap(), n);
                assert_eq!(r7.recv().await.unwrap(), n);
                assert_eq!(r8.recv().await.unwrap(), n);
                assert_eq!(r9.recv().await.unwrap(), n);
                assert_eq!(r10.recv().await.unwrap(), n);
                assert_eq!(r11.recv().await.unwrap(), n);
                assert_eq!(r12.recv().await.unwrap(), n);
                assert_eq!(r13.recv().await.unwrap(), n);
                assert_eq!(r14.recv().await.unwrap(), n);
                assert_eq!(r15.recv().await.unwrap(), n);
                assert_eq!(r16.recv().await.unwrap(), n);
                n += 1;
            })
        })
    });
}

criterion_group!(benches, broadcast_and_recv);
criterion_main!(benches);
