use benchmarks::sync_shared;
use benchmarks::Run;

#[test]
#[ignore]
fn run_throughput_bench() {
    for i in 0..15 {
        sync_shared::run_throughput_benchmark::<colock::mutex::Mutex<f64>>(
            &Run::from((2, 1, 1)),
            618222,
        );
        println!("done {}", i);
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
    }
}

#[test]
#[ignore]
fn run_latency_bench() {
    for i in 0..15 {
        sync_shared::run_latency_benchmark::<colock::mutex::Mutex<f64>>(
            &Run::from((2, 1, 1)),
            618222,
        );
        println!("done {}", i);
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
    }
}
