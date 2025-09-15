use plotters::prelude::*;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio::time::sleep;

const API_DHT_PUT: u16 = 650;
const API_DHT_GET: u16 = 651;
const API_DHT_SUCCESS: u16 = 652;

#[derive(Debug, Clone)]
struct BenchmarkConfig {
    host: String,
    port: u16,
    num_operations: usize,
    concurrency: usize,
    key_size: usize,
    value_size: usize,
    ttl: u16,
    replication: u8,
    read_ratio: f64,
    warmup_operations: usize,
    output_dir: String,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7401,
            num_operations: 1000,
            concurrency: 10,
            key_size: 16,
            value_size: 256,
            ttl: 300,
            replication: 1,
            read_ratio: 0.5,
            warmup_operations: 50,
            output_dir: "benchmark_results".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct BenchmarkResults {
    total_operations: usize,
    successful_operations: usize,
    failed_operations: usize,
    duration: Duration,
    operations_per_second: f64,
    avg_latency_ms: f64,
    min_latency_ms: f64,
    max_latency_ms: f64,
    p50_latency_ms: f64,
    p95_latency_ms: f64,
    p99_latency_ms: f64,
    latency_samples: Vec<f64>,
    read_latencies: Vec<f64>,
    write_latencies: Vec<f64>,
    throughput_over_time: Vec<(f64, f64)>,
    concurrency_impact: Vec<(usize, f64)>,
}

struct DHTBenchmark {
    config: BenchmarkConfig,
}

impl DHTBenchmark {
    fn new(config: BenchmarkConfig) -> Self {
        Self { config }
    }

    fn hash_key_bytes(key: &str) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let result = hasher.finalize();
        result.into()
    }

    fn generate_key(&self, index: usize) -> String {
        format!(
            "bench_key_{:0width$}",
            index,
            width = self.config.key_size.saturating_sub(10)
        )
    }

    fn generate_value(&self, index: usize) -> String {
        format!(
            "bench_value_{:0width$}",
            index,
            width = self.config.value_size.saturating_sub(12)
        )
    }

    fn put_operation_sync(&self, key: String, value: String) -> Result<f64, String> {
        let start = Instant::now();

        let mut stream = TcpStream::connect(format!("{}:{}", self.config.host, self.config.port))
            .map_err(|e| format!("Connection failed: {}", e))?;

        let key_bytes = Self::hash_key_bytes(&key);
        let value_bytes = value.as_bytes();

        let total_size = 4u16 + 2 + 1 + 1 + 32 + value_bytes.len() as u16;

        // Write header and payload (matching original benchmark format)
        stream
            .write_all(&total_size.to_be_bytes())
            .map_err(|e| format!("Write total_size failed: {}", e))?;
        stream
            .write_all(&API_DHT_PUT.to_be_bytes())
            .map_err(|e| format!("Write API_DHT_PUT failed: {}", e))?;
        stream
            .write_all(&self.config.ttl.to_be_bytes())
            .map_err(|e| format!("Write TTL failed: {}", e))?;
        stream
            .write_all(&[self.config.replication])
            .map_err(|e| format!("Write replication failed: {}", e))?;
        stream
            .write_all(&[0u8])
            .map_err(|e| format!("Write reserved failed: {}", e))?; // reserved
        stream
            .write_all(&key_bytes)
            .map_err(|e| format!("Write key failed: {}", e))?;
        stream
            .write_all(value_bytes)
            .map_err(|e| format!("Write value failed: {}", e))?;

        let latency = start.elapsed().as_secs_f64() * 1000.0;
        Ok(latency)
    }

    fn get_operation_sync(&self, key: String) -> Result<f64, String> {
        let start = Instant::now();

        let mut stream = TcpStream::connect(format!("{}:{}", self.config.host, self.config.port))
            .map_err(|e| format!("Connection failed: {}", e))?;

        let key_bytes = Self::hash_key_bytes(&key);
        let total_size = 4u16 + 2 + 32;

        // Write header and payload (matching original benchmark format)
        stream
            .write_all(&total_size.to_be_bytes())
            .map_err(|e| format!("Write total_size failed: {}", e))?;
        stream
            .write_all(&API_DHT_GET.to_be_bytes())
            .map_err(|e| format!("Write API_DHT_GET failed: {}", e))?;
        stream
            .write_all(&key_bytes)
            .map_err(|e| format!("Write key failed: {}", e))?;

        let latency = start.elapsed().as_secs_f64() * 1000.0;
        Ok(latency)
    }

    async fn warmup(&self) -> Result<(), String> {
        println!(
            "🔥 Warming up with {} operations...",
            self.config.warmup_operations
        );

        for i in 0..self.config.warmup_operations {
            let key = self.generate_key(i);
            let value = self.generate_value(i);

            let benchmark = DHTBenchmark::new(self.config.clone());
            let _ =
                tokio::task::spawn_blocking(move || benchmark.put_operation_sync(key, value)).await;

            if i % 10 == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }

        println!("✅ Warmup completed");
        Ok(())
    }

    async fn run_benchmark(&self) -> Result<BenchmarkResults, String> {
        self.warmup().await?;

        println!(
            "🚀 Starting benchmark with {} operations using {} concurrent connections...",
            self.config.num_operations, self.config.concurrency
        );

        let semaphore = Arc::new(Semaphore::new(self.config.concurrency));
        let successful_ops = Arc::new(AtomicU64::new(0));
        let failed_ops = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();

        let start_time = Instant::now();

        let mut read_latencies = Vec::new();
        let mut write_latencies = Vec::new();
        let mut throughput_samples = Vec::new();
        let ops_completed = Arc::new(AtomicU64::new(0));

        // Spawn throughput monitoring
        let num_operations = self.config.num_operations;
        let throughput_monitor = {
            let ops_completed = ops_completed.clone();
            let start_time = start_time.clone();
            tokio::spawn(async move {
                let mut samples = Vec::new();
                let mut last_count = 0;
                loop {
                    sleep(Duration::from_millis(200)).await;
                    let current_count = ops_completed.load(Ordering::Relaxed);
                    let elapsed = start_time.elapsed().as_secs_f64();
                    if current_count > last_count {
                        let throughput = (current_count - last_count) as f64 / 0.2;
                        samples.push((elapsed, throughput));
                        last_count = current_count;
                    }
                    if current_count >= num_operations as u64 {
                        break;
                    }
                }
                samples
            })
        };

        for i in 0..self.config.num_operations {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let successful_ops = successful_ops.clone();
            let failed_ops = failed_ops.clone();
            let ops_completed = ops_completed.clone();
            let config = self.config.clone();

            let handle = tokio::spawn(async move {
                let _permit = permit;

                let is_read = rand::random::<f64>() < config.read_ratio;
                let benchmark = DHTBenchmark::new(config.clone());
                let key = benchmark.generate_key(i);

                let result = if is_read {
                    tokio::task::spawn_blocking(move || benchmark.get_operation_sync(key))
                        .await
                        .unwrap()
                } else {
                    let value = benchmark.generate_value(i);
                    tokio::task::spawn_blocking(move || benchmark.put_operation_sync(key, value))
                        .await
                        .unwrap()
                };

                ops_completed.fetch_add(1, Ordering::Relaxed);

                match result {
                    Ok(latency) => {
                        successful_ops.fetch_add(1, Ordering::Relaxed);
                        Some((latency, is_read))
                    }
                    Err(_) => {
                        failed_ops.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            });

            handles.push(handle);
        }

        let mut latency_samples = Vec::new();
        for handle in handles {
            if let Ok(Some((latency, is_read))) = handle.await {
                latency_samples.push(latency);
                if is_read {
                    read_latencies.push(latency);
                } else {
                    write_latencies.push(latency);
                }
            }
        }

        throughput_samples = throughput_monitor.await.unwrap();

        let duration = start_time.elapsed();
        let successful_operations = successful_ops.load(Ordering::Relaxed) as usize;
        let failed_operations = failed_ops.load(Ordering::Relaxed) as usize;
        let operations_per_second = successful_operations as f64 / duration.as_secs_f64();

        // Calculate latency statistics
        latency_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let avg_latency_ms = if !latency_samples.is_empty() {
            latency_samples.iter().sum::<f64>() / latency_samples.len() as f64
        } else {
            0.0
        };
        let min_latency_ms = latency_samples.first().copied().unwrap_or(0.0);
        let max_latency_ms = latency_samples.last().copied().unwrap_or(0.0);

        let p50_idx = (latency_samples.len() as f64 * 0.5) as usize;
        let p95_idx = (latency_samples.len() as f64 * 0.95) as usize;
        let p99_idx = (latency_samples.len() as f64 * 0.99) as usize;

        let p50_latency_ms = latency_samples
            .get(p50_idx.saturating_sub(1))
            .copied()
            .unwrap_or(0.0);
        let p95_latency_ms = latency_samples
            .get(p95_idx.saturating_sub(1))
            .copied()
            .unwrap_or(0.0);
        let p99_latency_ms = latency_samples
            .get(p99_idx.saturating_sub(1))
            .copied()
            .unwrap_or(0.0);

        // Test different concurrency levels for concurrency impact
        let mut concurrency_impact = Vec::new();
        for &conc in &[1, 2, 5, 10, 20] {
            if conc <= self.config.concurrency {
                let mini_test = DHTBenchmark::new(BenchmarkConfig {
                    concurrency: conc,
                    num_operations: 50,
                    warmup_operations: 5,
                    ..self.config.clone()
                });
                if let Ok(mini_result) = mini_test.run_mini_benchmark().await {
                    concurrency_impact.push((conc, mini_result.operations_per_second));
                }
            }
        }

        Ok(BenchmarkResults {
            total_operations: self.config.num_operations,
            successful_operations,
            failed_operations,
            duration,
            operations_per_second,
            avg_latency_ms,
            min_latency_ms,
            max_latency_ms,
            p50_latency_ms,
            p95_latency_ms,
            p99_latency_ms,
            latency_samples,
            read_latencies,
            write_latencies,
            throughput_over_time: throughput_samples,
            concurrency_impact,
        })
    }

    fn print_results(&self, results: &BenchmarkResults) {
        println!("\n📊 BENCHMARK RESULTS");
        println!("═══════════════════════════════════════");
        println!("Total Operations:     {}", results.total_operations);
        println!(
            "Successful:           {} ({:.1}%)",
            results.successful_operations,
            (results.successful_operations as f64 / results.total_operations as f64) * 100.0
        );
        println!(
            "Failed:               {} ({:.1}%)",
            results.failed_operations,
            (results.failed_operations as f64 / results.total_operations as f64) * 100.0
        );
        println!();
        println!("Performance:");
        println!(
            "  Duration:           {:.2}s",
            results.duration.as_secs_f64()
        );
        println!(
            "  Throughput:         {:.2} ops/sec",
            results.operations_per_second
        );
        println!();
        println!("Latency (ms):");
        println!("  Average:            {:.2}", results.avg_latency_ms);
        println!("  Minimum:            {:.2}", results.min_latency_ms);
        println!("  Maximum:            {:.2}", results.max_latency_ms);
        println!("  50th percentile:    {:.2}", results.p50_latency_ms);
        println!("  95th percentile:    {:.2}", results.p95_latency_ms);
        println!("  99th percentile:    {:.2}", results.p99_latency_ms);
        println!();
        println!("Configuration:");
        println!("  Concurrency:        {}", self.config.concurrency);
        println!("  Key size:           {} bytes", self.config.key_size);
        println!("  Value size:         {} bytes", self.config.value_size);
        println!(
            "  Read ratio:         {:.1}%",
            self.config.read_ratio * 100.0
        );
        println!("  TTL:                {}s", self.config.ttl);
        println!("  Replication:        {}", self.config.replication);
    }

    fn generate_graphs(
        &self,
        results: &BenchmarkResults,
    ) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(&self.config.output_dir)?;

        self.create_latency_histogram(results)?;
        self.create_read_vs_write_latency(results)?;
        self.create_throughput_over_time(results)?;
        self.create_concurrency_scaling(results)?;

        println!("\n📈 Graphs generated in '{}/'", self.config.output_dir);
        println!("   - latency_histogram.png");
        println!("   - read_vs_write_latency.png");
        println!("   - throughput_over_time.png");
        println!("   - concurrency_scaling.png");

        Ok(())
    }

    fn create_latency_histogram(
        &self,
        results: &BenchmarkResults,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if results.latency_samples.is_empty() {
            return Ok(());
        }

        let path = format!("{}/latency_histogram.png", self.config.output_dir);
        let root = BitMapBackend::new(&path, (800, 600)).into_drawing_area();
        root.fill(&WHITE)?;

        let max_latency = results.max_latency_ms;
        let bin_count = 30;
        let bin_size = max_latency / bin_count as f64;

        let mut histogram = vec![0; bin_count];
        for &latency in &results.latency_samples {
            let bin = ((latency / bin_size) as usize).min(bin_count - 1);
            histogram[bin] += 1;
        }

        let max_count = histogram.iter().max().unwrap_or(&1);

        let mut chart = ChartBuilder::on(&root)
            .caption("Latency Distribution", ("Arial", 30))
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(50)
            .build_cartesian_2d(0.0..max_latency, 0..*max_count)?;

        chart
            .configure_mesh()
            .x_desc("Latency (ms)")
            .y_desc("Frequency")
            .draw()?;

        chart.draw_series(histogram.iter().enumerate().map(|(i, &count)| {
            Rectangle::new(
                [(i as f64 * bin_size, 0), ((i + 1) as f64 * bin_size, count)],
                BLUE.filled(),
            )
        }))?;

        root.present()?;
        Ok(())
    }

    fn create_read_vs_write_latency(
        &self,
        results: &BenchmarkResults,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if results.read_latencies.is_empty() && results.write_latencies.is_empty() {
            return Ok(());
        }

        let path = format!("{}/read_vs_write_latency.png", self.config.output_dir);
        let root = BitMapBackend::new(&path, (800, 600)).into_drawing_area();
        root.fill(&WHITE)?;

        let max_latency = results
            .latency_samples
            .iter()
            .fold(0.0f64, |a, &b| a.max(b))
            * 1.1;

        let mut chart = ChartBuilder::on(&root)
            .caption("Read vs Write Latency Distribution", ("Arial", 25))
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(50)
            .build_cartesian_2d(0.0..max_latency, 0..100)?;

        chart
            .configure_mesh()
            .x_desc("Latency (ms)")
            .y_desc("Frequency")
            .draw()?;

        // Create histograms for read and write latencies
        let bin_count = 30;
        let bin_size = max_latency / bin_count as f64;

        if !results.read_latencies.is_empty() {
            let mut read_hist = vec![0; bin_count];
            for &latency in &results.read_latencies {
                let bin = ((latency / bin_size) as usize).min(bin_count - 1);
                read_hist[bin] += 1;
            }

            chart
                .draw_series(read_hist.iter().enumerate().map(|(i, &count)| {
                    Rectangle::new(
                        [(i as f64 * bin_size, 0), ((i + 1) as f64 * bin_size, count)],
                        BLUE.mix(0.7).filled(),
                    )
                }))?
                .label("READ Operations")
                .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 10, y)], &BLUE));
        }

        if !results.write_latencies.is_empty() {
            let mut write_hist = vec![0; bin_count];
            for &latency in &results.write_latencies {
                let bin = ((latency / bin_size) as usize).min(bin_count - 1);
                write_hist[bin] += 1;
            }

            chart
                .draw_series(write_hist.iter().enumerate().map(|(i, &count)| {
                    Rectangle::new(
                        [
                            (i as f64 * bin_size, count),
                            ((i + 1) as f64 * bin_size, count * 2),
                        ],
                        RED.mix(0.7).filled(),
                    )
                }))?
                .label("WRITE Operations")
                .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 10, y)], &RED));
        }

        chart.configure_series_labels().draw()?;
        root.present()?;
        Ok(())
    }

    fn create_throughput_over_time(
        &self,
        results: &BenchmarkResults,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if results.throughput_over_time.is_empty() {
            return Ok(());
        }

        let path = format!("{}/throughput_over_time.png", self.config.output_dir);
        let root = BitMapBackend::new(&path, (800, 600)).into_drawing_area();
        root.fill(&WHITE)?;

        let max_time = results
            .throughput_over_time
            .iter()
            .map(|(t, _)| *t)
            .fold(0.0, f64::max);
        let max_throughput = results
            .throughput_over_time
            .iter()
            .map(|(_, t)| *t)
            .fold(0.0, f64::max)
            * 1.1;

        let mut chart = ChartBuilder::on(&root)
            .caption("DHT Throughput Over Time", ("Arial", 25))
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(50)
            .build_cartesian_2d(0f64..max_time, 0f64..max_throughput)?;

        chart
            .configure_mesh()
            .x_desc("Time (seconds)")
            .y_desc("Operations per Second")
            .draw()?;

        chart
            .draw_series(LineSeries::new(
                results
                    .throughput_over_time
                    .iter()
                    .map(|(t, thr)| (*t, *thr)),
                BLUE.stroke_width(2),
            ))?
            .label("Instantaneous Throughput")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 10, y)], &BLUE));

        // Add average line
        chart
            .draw_series(LineSeries::new(
                vec![
                    (0.0, results.operations_per_second),
                    (max_time, results.operations_per_second),
                ],
                RED.stroke_width(2),
            ))?
            .label(format!(
                "Average: {:.1} ops/sec",
                results.operations_per_second
            ))
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 10, y)], &RED));

        chart.configure_series_labels().draw()?;
        root.present()?;
        Ok(())
    }

    fn create_concurrency_scaling(
        &self,
        results: &BenchmarkResults,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if results.concurrency_impact.is_empty() {
            return Ok(());
        }

        let path = format!("{}/concurrency_scaling.png", self.config.output_dir);
        let root = BitMapBackend::new(&path, (800, 600)).into_drawing_area();
        root.fill(&WHITE)?;

        let max_concurrency = results
            .concurrency_impact
            .iter()
            .map(|(c, _)| *c)
            .max()
            .unwrap_or(1) as f64;
        let max_throughput = results
            .concurrency_impact
            .iter()
            .map(|(_, t)| *t)
            .fold(0.0, f64::max)
            * 1.1;

        let mut chart = ChartBuilder::on(&root)
            .caption("DHT Concurrency Scaling", ("Arial", 25))
            .margin(10)
            .x_label_area_size(40)
            .y_label_area_size(50)
            .build_cartesian_2d(0f64..max_concurrency, 0f64..max_throughput)?;

        chart
            .configure_mesh()
            .x_desc("Concurrent Connections")
            .y_desc("Throughput (ops/sec)")
            .draw()?;

        chart
            .draw_series(results.concurrency_impact.iter().map(|(conc, throughput)| {
                Circle::new((*conc as f64, *throughput), 5, BLUE.filled())
            }))?
            .label("Measured Throughput")
            .legend(|(x, y)| Circle::new((x + 5, y), 3, BLUE.filled()));

        chart
            .draw_series(LineSeries::new(
                results
                    .concurrency_impact
                    .iter()
                    .map(|(conc, throughput)| (*conc as f64, *throughput)),
                GREEN.stroke_width(2),
            ))?
            .label("Scaling Curve")
            .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 10, y)], &GREEN));

        chart.configure_series_labels().draw()?;
        root.present()?;
        Ok(())
    }

    async fn run_mini_benchmark(&self) -> Result<BenchmarkResults, String> {
        let semaphore = Arc::new(Semaphore::new(self.config.concurrency));
        let successful_ops = Arc::new(AtomicU64::new(0));
        let failed_ops = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();

        let start_time = Instant::now();

        for i in 0..self.config.num_operations {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let successful_ops = successful_ops.clone();
            let failed_ops = failed_ops.clone();
            let config = self.config.clone();

            let handle = tokio::spawn(async move {
                let _permit = permit;
                let benchmark = DHTBenchmark::new(config.clone());
                let key = benchmark.generate_key(i);
                let value = benchmark.generate_value(i);

                let result =
                    tokio::task::spawn_blocking(move || benchmark.put_operation_sync(key, value))
                        .await
                        .unwrap();

                match result {
                    Ok(_) => {
                        successful_ops.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        failed_ops.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });

            handles.push(handle);
        }

        for handle in handles {
            let _ = handle.await;
        }

        let duration = start_time.elapsed();
        let successful_operations = successful_ops.load(Ordering::Relaxed) as usize;
        let failed_operations = failed_ops.load(Ordering::Relaxed) as usize;
        let operations_per_second = successful_operations as f64 / duration.as_secs_f64();

        Ok(BenchmarkResults {
            total_operations: self.config.num_operations,
            successful_operations,
            failed_operations,
            duration,
            operations_per_second,
            avg_latency_ms: 0.0,
            min_latency_ms: 0.0,
            max_latency_ms: 0.0,
            p50_latency_ms: 0.0,
            p95_latency_ms: 0.0,
            p99_latency_ms: 0.0,
            latency_samples: Vec::new(),
            read_latencies: Vec::new(),
            write_latencies: Vec::new(),
            throughput_over_time: Vec::new(),
            concurrency_impact: Vec::new(),
        })
    }
}

fn print_help() {
    println!("DHT Chord Benchmark Tool with Graphs");
    println!("Usage: dht_benchmark_graphs [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -h, --host <HOST>           DHT node hostname (default: 127.0.0.1)");
    println!("  -p, --port <PORT>           DHT node API port (default: 7401)");
    println!("  -n, --operations <NUM>      Number of operations (default: 1000)");
    println!("  -c, --concurrency <NUM>     Concurrent connections (default: 10)");
    println!("  -k, --key-size <BYTES>      Key size in bytes (default: 16)");
    println!("  -v, --value-size <BYTES>    Value size in bytes (default: 256)");
    println!("  -r, --read-ratio <RATIO>    Read ratio 0.0-1.0 (default: 0.5)");
    println!("  -t, --ttl <SECONDS>         TTL for stored values (default: 300)");
    println!("  --replication <NUM>         Replication factor (default: 1)");
    println!(
        "  --output-dir <DIR>          Output directory for graphs (default: benchmark_results)"
    );
    println!("  --preset <PRESET>           Use predefined configuration");
    println!("  --help                      Show this help message");
    println!();
    println!("Presets:");
    println!("  quick      - Fast test (100 ops, 5 concurrency)");
    println!("  standard   - Standard test (1000 ops, 10 concurrency)");
    println!("  stress     - Stress test (5000 ops, 20 concurrency)");
}

fn parse_args() -> Result<BenchmarkConfig, String> {
    let args: Vec<String> = std::env::args().collect();
    let mut config = BenchmarkConfig::default();
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-h" | "--host" => {
                i += 1;
                if i >= args.len() {
                    return Err("Missing host value".to_string());
                }
                config.host = args[i].clone();
            }
            "-p" | "--port" => {
                i += 1;
                if i >= args.len() {
                    return Err("Missing port value".to_string());
                }
                config.port = args[i]
                    .parse()
                    .map_err(|_| "Invalid port number".to_string())?;
            }
            "-n" | "--operations" => {
                i += 1;
                if i >= args.len() {
                    return Err("Missing operations value".to_string());
                }
                config.num_operations = args[i]
                    .parse()
                    .map_err(|_| "Invalid operations number".to_string())?;
            }
            "-c" | "--concurrency" => {
                i += 1;
                if i >= args.len() {
                    return Err("Missing concurrency value".to_string());
                }
                config.concurrency = args[i]
                    .parse()
                    .map_err(|_| "Invalid concurrency number".to_string())?;
            }
            "--output-dir" => {
                i += 1;
                if i >= args.len() {
                    return Err("Missing output-dir value".to_string());
                }
                config.output_dir = args[i].clone();
            }
            "--preset" => {
                i += 1;
                if i >= args.len() {
                    return Err("Missing preset value".to_string());
                }
                match args[i].as_str() {
                    "quick" => {
                        config.num_operations = 100;
                        config.concurrency = 5;
                        config.warmup_operations = 20;
                    }
                    "standard" => {
                        config.num_operations = 1000;
                        config.concurrency = 10;
                        config.warmup_operations = 50;
                    }
                    "stress" => {
                        config.num_operations = 5000;
                        config.concurrency = 20;
                        config.warmup_operations = 100;
                    }
                    _ => return Err(format!("Unknown preset: {}", args[i])),
                }
            }
            _ => {
                if args[i].starts_with('-') {
                    return Err(format!("Unknown option: {}", args[i]));
                }
            }
        }
        i += 1;
    }

    Ok(config)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let config = parse_args().unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        eprintln!("Use --help for usage information");
        std::process::exit(1);
    });

    println!("🚀 DHT Chord Benchmark Tool with Graphs");
    println!("Connecting to {}:{}", config.host, config.port);

    let benchmark = DHTBenchmark::new(config);

    match benchmark.run_benchmark().await {
        Ok(results) => {
            benchmark.print_results(&results);

            println!("\n📈 Generating graphs...");
            if let Err(e) = benchmark.generate_graphs(&results) {
                eprintln!("⚠️ Failed to generate graphs: {}", e);
            }
        }
        Err(e) => {
            eprintln!("❌ Benchmark failed: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
