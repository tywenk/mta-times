use anyhow::Result;
use chrono::{Duration as ChronoDuration, Utc as ChronoUtc};
use prost::Message;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time;

mod subway;
use subway::FeedMessage;

// This file includes most, but not all, service changes for the next seven calendar days.
// Generally, the 'simpler' the service change, the more likely it will not be included.
// Beyond that period, service changes will not be included. It is updated hourly.
const _SUPPLEMENTED_GTFS_URL: &str = "https://rrgtfsfeeds.s3.amazonaws.com/gtfs_supplemented.zip";

// This file represents the "normal" subway schedule and does not include most temporary service
// changes, though some long term service changes may be included. It is typically updated a few times a year.
const GTFS_URL: &str = "https://rrgtfsfeeds.s3.amazonaws.com/gtfs_subway.zip";

// MTA GTFS-Realtime feed URLs. These endpoints require an API key.
// The base URL is for the numbered lines (1, 2, 3, 4, 5, 6, 7)
const MTA_SUBWAY_FEED_URL: &str =
    "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs";
const SUFFIX_ACE: &str = "ace";
const SUFFIX_BDFM: &str = "bdfm";
const SUFFIX_G: &str = "g";
const SUFFIX_JZ: &str = "jz";
const SUFFIX_NQRW: &str = "nqrw";
const SUFFIX_L: &str = "l";
const SUFFIX_SIR: &str = "si";

/// Fetches the GTFS data. This is used to get the list of stops and routes.
async fn fetch_supplemented_gtfs() -> Result<gtfs_structures::Gtfs> {
    println!("Fetching latest GTFS data...");
    let gtfs = gtfs_structures::Gtfs::from_url_async(GTFS_URL).await?;
    gtfs.print_stats();
    Ok(gtfs)
}

/// Maps route IDs to their corresponding MTA realtime feed endpoints
fn get_realtime_feeds_for_routes(routes: &HashSet<String>) -> Result<Vec<String>> {
    let mut feeds = Vec::new();

    // If a route ends is 'X', is it an express route.
    // Strip the trailing 'X' from the route ID, if it exists, since it uses the same feed as the base route.
    let routes: Vec<&str> = routes
        .iter()
        .map(|route| route.trim_end_matches('X'))
        .collect();

    for route in routes {
        match route {
            "A" | "C" | "E" => {
                if !feeds.contains(&SUFFIX_ACE.to_string()) {
                    feeds.push(SUFFIX_ACE.to_string());
                }
            }
            "B" | "D" | "F" | "M" => {
                if !feeds.contains(&SUFFIX_BDFM.to_string()) {
                    feeds.push(SUFFIX_BDFM.to_string());
                }
            }
            "G" => {
                if !feeds.contains(&SUFFIX_G.to_string()) {
                    feeds.push(SUFFIX_G.to_string());
                }
            }
            "J" | "Z" => {
                if !feeds.contains(&SUFFIX_JZ.to_string()) {
                    feeds.push(SUFFIX_JZ.to_string());
                }
            }
            "N" | "Q" | "R" | "W" => {
                if !feeds.contains(&SUFFIX_NQRW.to_string()) {
                    feeds.push(SUFFIX_NQRW.to_string());
                }
            }
            "L" => {
                if !feeds.contains(&SUFFIX_L.to_string()) {
                    feeds.push(SUFFIX_L.to_string());
                }
            }
            "SI" => {
                if !feeds.contains(&SUFFIX_SIR.to_string()) {
                    feeds.push(SUFFIX_SIR.to_string());
                }
            }
            "1" | "2" | "3" | "4" | "5" | "6" | "7" => {
                // These routes use the base URL without suffix
                if !feeds.contains(&"".to_string()) {
                    feeds.push("".to_string());
                }
            }
            _ => {
                // Unknown route, return error
                return Err(anyhow::anyhow!("Unknown route: {}", route));
            }
        }
    }

    Ok(feeds)
}

/// Gets all routes that serve a specific stop
fn get_routes_for_stop(gtfs: &gtfs_structures::Gtfs, stop_id: &str) -> HashSet<String> {
    let mut routes = HashSet::new();

    for trip in gtfs.trips.values() {
        for stop_time in &trip.stop_times {
            if stop_time.stop.id == stop_id {
                routes.insert(trip.route_id.clone());
                break;
            }
        }
    }

    routes
}

async fn fetch_realtime_data(url: &str) -> Result<FeedMessage> {
    let mut request = reqwest::Client::new().get(url);
    request = request.header("Accept", "application/x-protobuf");
    let response = request.send().await?;

    // Check if the response is successful
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
    }

    let bytes = response.bytes().await?;
    let feed_message = FeedMessage::decode(&bytes[..])?;

    Ok(feed_message)
}

/// Fetches and combines realtime data from multiple MTA feeds
async fn fetch_combined_realtime_data(feeds: &[String]) -> Result<Vec<FeedMessage>> {
    // Create futures for all feeds using tokio::spawn to avoid lifetime issues
    let mut handles = Vec::new();
    for feed_suffix in feeds {
        let url = if feed_suffix.is_empty() {
            MTA_SUBWAY_FEED_URL.to_string()
        } else {
            format!("{}-{}", MTA_SUBWAY_FEED_URL, feed_suffix)
        };

        let handle = tokio::spawn(async move { fetch_realtime_data(&url).await });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    let mut feed_messages = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(Ok(feed)) => {
                feed_messages.push(feed);
            }
            Ok(Err(e)) => {
                println!("Failed to fetch feed: {}", e);
            }
            Err(e) => {
                println!("Task failed: {}", e);
            }
        }
    }

    Ok(feed_messages)
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

    // Get routes that serve this stop
    let routes = get_routes_for_stop(&gtfs, &stop_id);
    println!("Routes serving this stop: {:?}", routes);

    // Get the realtime feeds we need to fetch
    let feeds = get_realtime_feeds_for_routes(&routes)?;
    println!("Realtime feeds to fetch: {:?}", feeds);

    println!("Press Ctrl+C to exit\n");

    loop {
        check_trains_at_stop(&gtfs, &stop_id, &feeds).await?;
        time::sleep(Duration::from_secs(10)).await;
    }
}

async fn check_trains_at_stop(
    gtfs: &gtfs_structures::Gtfs,
    stop_id: &str,
    feeds: &[String],
) -> Result<()> {
    // Fetch realtime data from all required feeds
    let realtime_feeds = fetch_combined_realtime_data(feeds).await?;

    let mut route_times: HashMap<String, Vec<i32>> = HashMap::new();
    let current_timestamp = ChronoUtc::now().timestamp();

    // Process realtime data to find upcoming trains
    for feed in &realtime_feeds {
        for entity in &feed.entity {
            if let Some(trip_update) = &entity.trip_update {
                for stop_update in &trip_update.stop_time_update {
                    if let Some(stop_id_update) = &stop_update.stop_id {
                        if stop_id_update == stop_id {
                            // Found a train coming to our stop
                            if let Some(arrival) = &stop_update.arrival {
                                if let Some(arrival_time) = arrival.time {
                                    let arrival_timestamp = arrival_time as i64;
                                    let time_diff = (arrival_timestamp - current_timestamp) as i32;

                                    if time_diff > 0 {
                                        // Get route ID from trip descriptor
                                        if let Some(route_id) = &trip_update.trip.route_id {
                                            let route_name = route_id.clone();

                                            route_times
                                                .entry(route_name)
                                                .or_insert_with(Vec::new)
                                                .push(time_diff);
                                        }
                                    }
                                }
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
        for (route_id, times) in &route_times {
            // Sort times for this route and take the next 2
            let mut sorted_times = times.clone();
            sorted_times.sort();
            let next_trains = sorted_times.iter().take(2);

            if let Some(route) = gtfs.routes.get(route_id) {
                let route_name = route.short_name.as_deref().unwrap_or(route_id);
                println!("{} Train:", route_name);

                for (i, &seconds) in next_trains.enumerate() {
                    let future_time = ChronoUtc::now() + ChronoDuration::seconds(seconds as i64);
                    let human_time = chrono_humanize::HumanTime::from(future_time);
                    println!("  {}: {}", i + 1, human_time);
                }
            } else {
                println!("Route {}:", route_id);
                for (i, &seconds) in next_trains.enumerate() {
                    let future_time = ChronoUtc::now() + ChronoDuration::seconds(seconds as i64);
                    let human_time = chrono_humanize::HumanTime::from(future_time);
                    println!("  {}: {}", i + 1, human_time);
                }
            }
        }
    }

    println!("---");
    Ok(())
}
