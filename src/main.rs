use anyhow::Result;
use chrono::Timelike;
use prost::Message;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time;

mod subway;
use subway::FeedMessage;

// This file includes most, but not all, service changes for the next seven calendar days.
// Generally, the 'simpler' the service change, the more likely it will not be included.
// Beyond that period, service changes will not be included. It is updated hourly.
const SUPPLEMENTED_GTFS_URL: &str = "https://rrgtfsfeeds.s3.amazonaws.com/gtfs_supplemented.zip";

async fn fetch_supplemented_gtfs() -> Result<gtfs_structures::Gtfs> {
    println!("Fetching fetch_supplemented_gtfs...");
    let gtfs = gtfs_structures::Gtfs::from_url_async(SUPPLEMENTED_GTFS_URL).await?;
    gtfs.print_stats();
    Ok(gtfs)
}

// MTA GTFS-Realtime feed URLs. This endpoit does not need an API Key.
const _MTA_SUBWAY_FEED_URL: &str = "https://api-endpoint.mta.info/Dataservice/mtagtfsrealtime/gtfs";

async fn fetch_realtime_data(url: &str) -> Result<FeedMessage> {
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    let feed_message = FeedMessage::decode(&bytes[..])?;
    Ok(feed_message)
}

#[tokio::main]
async fn main() -> Result<()> {
    let gtfs = fetch_supplemented_gtfs().await?;

    let stop_id = if let Some(arg_stop_id) = std::env::args().nth(1) {
        if !gtfs.stops.contains_key(&arg_stop_id) {
            println!("Stop ID '{}' not found in GTFS data", arg_stop_id);
            return Ok(());
        }
        arg_stop_id
    } else {
        println!("Available stops:");
        let mut stops: Vec<_> = gtfs.stops.iter().collect();
        stops.sort_by_key(|&(id, _)| id);

        for (stop_id, stop) in stops {
            println!("{}: {}", stop_id, stop.name.as_deref().unwrap_or("Unknown"));
        }

        println!("\nEnter a stop ID:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if !gtfs.stops.contains_key(input) {
            println!("Stop ID '{}' not found in GTFS data", input);
            return Ok(());
        }

        input.to_string()
    };

    let stop = &gtfs.stops[&stop_id];
    println!(
        "Monitoring trains at: {} ({})",
        stop.name.as_deref().unwrap_or("Unknown"),
        stop_id
    );
    println!("Press Ctrl+C to exit\n");

    loop {
        check_trains_at_stop(&gtfs, &stop_id).await?;
        time::sleep(Duration::from_secs(10)).await;
    }
}

async fn check_trains_at_stop(gtfs: &gtfs_structures::Gtfs, stop_id: &str) -> Result<()> {
    let mut route_times: HashMap<String, i32> = HashMap::new();
    let current_time = chrono::Local::now().time();
    let current_seconds = current_time.num_seconds_from_midnight() as i32;

    println!("Debug: First 10 trip keys:");
    for (i, key) in gtfs.trips.keys().take(10).enumerate() {
        println!("  {}: {}", i + 1, key);
    }

    println!("\nDebug: First 10 trip values (route_id and service_id):");
    for (i, trip) in gtfs.trips.values().take(10).enumerate() {
        println!(
            "  {}: route_id={}, service_id={}",
            i + 1,
            trip.route_id,
            trip.service_id
        );
    }

    for trip in gtfs.trips.values() {
        for stop_time in &trip.stop_times {
            if stop_time.stop.id == stop_id {
                if let Some(arrival_time) = &stop_time.arrival_time {
                    let arrival_seconds = *arrival_time as i32;
                    let mut time_diff = arrival_seconds - current_seconds;

                    if time_diff < 0 {
                        time_diff += 24 * 3600;
                    }

                    let route_name = trip.route_id.clone();

                    match route_times.get(&route_name) {
                        None => {
                            route_times.insert(route_name, time_diff);
                        }
                        Some(&existing_time) => {
                            if time_diff < existing_time {
                                route_times.insert(route_name, time_diff);
                            }
                        }
                    }
                }
            }
        }
    }

    if route_times.is_empty() {
        println!("No upcoming trains found at this stop");
    } else {
        let mut sorted_routes: Vec<_> = route_times.iter().collect();
        sorted_routes.sort_by_key(|&(_, time)| time);

        for (route_id, &seconds) in sorted_routes {
            let minutes = seconds / 60;
            if let Some(route) = gtfs.routes.get(route_id) {
                println!(
                    "{} Train: {} minutes",
                    route.short_name.as_deref().unwrap_or(route_id),
                    minutes
                );
            } else {
                println!("Route {}: {} minutes", route_id, minutes);
            }
        }
    }

    println!("---");
    Ok(())
}
