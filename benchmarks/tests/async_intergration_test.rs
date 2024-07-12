// use benchmarks::async_shared;
// use benchmarks::Run;
//
// #[tokio::test(flavor = "multi_thread")]
// #[ignore]
// async fn run_bench() {
//     for i in 0..1 {
//         async_shared::run_throughput_benchmark::<colock::mutex::Mutex<f64>>(
//             Run::from((2, 1, 1)),
//             618222,
//         )
//         .await;
//         println!("done {}", i);
//         std::io::Write::flush(&mut std::io::stdout()).unwrap();
//     }
// }
