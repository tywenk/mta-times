use anyhow::Result;
use train_checker::{StopMonitor, StopStatus, TrainChecker, TrainCheckerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    let checker = TrainChecker::new().await?;
    checker.print_stats();

    let stop_id = if let Some(arg_stop_id) = std::env::args().nth(1) {
        if !checker.is_valid_stop(&arg_stop_id) {
            println!("Stop ID '{}' not found in GTFS data", arg_stop_id);
            return Ok(());
        }
        arg_stop_id
    } else {
        println!("Available stops:");
        let stops = checker.get_all_stops();

        for (stop_id, stop_name) in stops {
            println!("{}: {}", stop_id, stop_name.as_deref().unwrap_or("Unknown"));
        }

        println!("\nEnter a stop ID:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if !checker.is_valid_stop(input) {
            println!("Stop ID '{}' not found in GTFS data", input);
            return Ok(());
        }

        input.to_string()
    };

    let stop_name = checker.get_stop_name(&stop_id);
    println!(
        "Monitoring trains at: {} ({})",
        stop_name.as_deref().unwrap_or("Unknown"),
        stop_id
    );

    // Get routes that serve this stop
    let routes = checker.get_routes_for_stop(&stop_id);
    println!("Routes serving this stop: {:?}", routes);

    println!("Press Ctrl+C to exit\n");

    // Create monitor with default config
    let config = TrainCheckerConfig::default();
    let monitor = StopMonitor::new(config).await?;

    // Monitor the stop with a callback that prints the status
    monitor
        .monitor_stop(&stop_id, |status| {
            print_stop_status(&status);
        })
        .await?;

    Ok(())
}

fn print_stop_status(status: &StopStatus) {
    if status.train_arrivals.is_empty() {
        println!("No upcoming trains found at this stop");
    } else {
        for (route_id, arrivals) in &status.train_arrivals {
            if let Some(route_name) = arrivals.first().and_then(|a| a.route_name.as_ref()) {
                println!("{} Train:", route_name);
            } else {
                println!("Route {}:", route_id);
            }

            for (i, arrival) in arrivals.iter().enumerate() {
                println!("  {}: {}", i + 1, arrival.human_time);
            }
        }
    }

    println!("---");
}
