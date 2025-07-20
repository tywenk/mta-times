use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc as ChronoUtc};
use prost::Message;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

mod subway;
use subway::FeedMessage;

// This file represents the "normal" subway schedule and does not include most temporary service
// changes, though some long term service changes may be included. It is typically updated a few times a year.
const GTFS_URL: &str = "https://rrgtfsfeeds.s3.amazonaws.com/gtfs_subway.zip";

// MTA GTFS-Realtime feed URLs. These endpoints require an API key.
// The base URL is for the numbered lines (1, 2, 3, 4, 5, 6, 7)
const MTA_SUBWAY_FEED_URL: &str =
    "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs";

// Suffixes for different routes in the MTA GTFS-Realtime feed.
const SUFFIX_ACE: &str = "ace";
const SUFFIX_BDFM: &str = "bdfm";
const SUFFIX_G: &str = "g";
const SUFFIX_JZ: &str = "jz";
const SUFFIX_NQRW: &str = "nqrw";
const SUFFIX_L: &str = "l";
const SUFFIX_SIR: &str = "si";

/// Represents a train arrival with route and timing information
#[derive(Debug, Clone)]
pub struct TrainArrival {
    pub route_id: String,
    pub route_name: Option<String>,
    pub arrival_time: i32, // seconds from now
    pub human_time: String,
}

/// Represents the current state of a stop with upcoming trains
#[derive(Debug, Clone)]
pub struct StopStatus {
    pub stop_id: String,
    pub stop_name: Option<String>,
    pub routes: HashSet<String>,
    pub train_arrivals: HashMap<String, Vec<TrainArrival>>, // route_id -> [TrainArrival]
}

/// Core train checker that manages GTFS data and realtime feeds
pub struct TrainChecker {
    gtfs: gtfs_structures::Gtfs,
    stop_name_to_id: HashMap<String, String>,
    stop_id_to_name: HashMap<String, String>,
    failed_requests: AtomicU32,
}

pub enum TrainCheckerStatus {
    Ok,
    Error,
}

impl TrainChecker {
    /// Creates a new TrainChecker instance by fetching GTFS data
    pub async fn new() -> Result<Self> {
        let gtfs = Self::fetch_gtfs_data().await?;

        // Build lookup maps for efficient stop name/ID lookups
        let mut stop_name_to_id = HashMap::new();
        let mut stop_id_to_name = HashMap::new();
        for (id, stop) in &gtfs.stops {
            if let Some(name) = &stop.name {
                stop_name_to_id.insert(name.clone(), id.clone());
                stop_id_to_name.insert(id.clone(), name.clone());
            }
        }

        Ok(Self {
            gtfs,
            stop_name_to_id,
            stop_id_to_name,
            failed_requests: AtomicU32::new(0),
        })
    }

    pub fn get_failed_requests_count(&self) -> u32 {
        self.failed_requests.load(Ordering::Relaxed)
    }

    pub fn reset_failed_requests(&self) {
        self.failed_requests.store(0, Ordering::Relaxed);
    }

    pub fn get_status(&self) -> TrainCheckerStatus {
        if self.get_failed_requests_count() > 10 {
            TrainCheckerStatus::Error
        } else {
            TrainCheckerStatus::Ok
        }
    }

    /// Fetches the GTFS data. This is used to get the list of stops and routes.
    async fn fetch_gtfs_data() -> Result<gtfs_structures::Gtfs> {
        let gtfs = gtfs_structures::Gtfs::from_url_async(GTFS_URL)
            .await
            .context("Failed to fetch GTFS data from MTA feed")?;
        Ok(gtfs)
    }

    /// Gets all available stops with their names
    pub fn get_all_stops(&self) -> Vec<(String, Option<String>)> {
        let mut stops: Vec<_> = self.gtfs.stops.iter().collect();
        stops.sort_by_key(|&(id, _)| id);
        stops
            .into_iter()
            .map(|(id, stop)| (id.clone(), stop.name.clone()))
            .collect()
    }

    /// Validates if a stop ID exists
    pub fn is_valid_stop(&self, stop_id: &str) -> bool {
        self.gtfs.stops.contains_key(stop_id)
    }

    /// Gets the stop name for a given stop ID
    pub fn get_stop_name(&self, stop_id: &str) -> Option<String> {
        self.stop_id_to_name.get(stop_id).cloned()
    }

    /// Gets the stop ID for a given stop name
    pub fn get_stop_id(&self, stop_name: &str) -> Option<String> {
        self.stop_name_to_id.get(stop_name).cloned()
    }

    /// Gets all routes that serve a specific stop
    pub fn get_routes_for_stop(&self, stop_id: &str) -> HashSet<String> {
        let mut routes = HashSet::new();

        for trip in self.gtfs.trips.values() {
            for stop_time in &trip.stop_times {
                if stop_time.stop.id == stop_id {
                    routes.insert(trip.route_id.clone());
                    break;
                }
            }
        }

        routes
    }

    /// Maps route IDs to their corresponding MTA realtime feed endpoints
    fn get_realtime_feeds_for_routes(&self, routes: &HashSet<String>) -> Result<Vec<String>> {
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

    /// Fetches realtime data from a single MTA feed
    async fn fetch_realtime_data(url: &str) -> Result<FeedMessage> {
        let mut request = reqwest::Client::new().get(url);
        request = request.header("Accept", "application/x-protobuf");
        let response = request
            .send()
            .await
            .context("Failed to fetch realtime data")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", response.status()));
        }

        let bytes = response
            .bytes()
            .await
            .context("Failed to read realtime response bytes")?;

        let feed_message = FeedMessage::decode(bytes.as_ref())
            .context("Failed to decode realtime protobuf message")?;

        Ok(feed_message)
    }

    /// Fetches and combines realtime data from multiple MTA feeds
    async fn fetch_combined_realtime_data(&self, feeds: &[String]) -> Result<Vec<FeedMessage>> {
        if feeds.is_empty() {
            return Err(anyhow::anyhow!("No feeds provided for realtime data"));
        }

        // Make parallel requests to the feeds.
        let mut handles = Vec::new();
        for feed_suffix in feeds {
            let url = if feed_suffix.is_empty() {
                MTA_SUBWAY_FEED_URL.to_string()
            } else {
                // The MTA feed url appends the feed suffix to the base url with a hyphen.
                format!("{}-{}", MTA_SUBWAY_FEED_URL, feed_suffix)
            };

            let handle = tokio::spawn(async move { Self::fetch_realtime_data(&url).await });
            handles.push(handle);
        }

        let mut feed_messages = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(feed)) => {
                    feed_messages.push(feed);
                }
                Ok(Err(e)) => {
                    eprintln!("Failed to fetch feed: {}", e);
                    self.failed_requests.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    eprintln!("Task failed: {}", e);
                    self.failed_requests.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        Ok(feed_messages)
    }

    /// Gets the current status of a stop with upcoming train arrivals
    pub async fn get_stop_status(&self, stop_id: &str) -> Result<StopStatus> {
        if !self.is_valid_stop(stop_id) {
            return Err(anyhow::anyhow!("Invalid stop ID: {}", stop_id));
        }

        let routes = self.get_routes_for_stop(stop_id);
        let feeds = self.get_realtime_feeds_for_routes(&routes)?;
        let realtime_feeds = self.fetch_combined_realtime_data(&feeds).await?;

        // Map of route ID to a list of arrival times.
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
                                        let time_diff =
                                            arrival_time as i32 - current_timestamp as i32;
                                        if time_diff > 0 {
                                            // Get route ID from trip descriptor
                                            if let Some(route_id) = &trip_update.trip.route_id {
                                                route_times
                                                    .entry(route_id.clone())
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

        // Convert to TrainArrival objects
        // Map of route ID to a list of TrainArrival objects.
        let mut train_arrivals: HashMap<String, Vec<TrainArrival>> = HashMap::new();
        for (route_id, times) in route_times {
            let mut sorted_times = times.clone();
            sorted_times.sort();
            let next_trains = sorted_times.iter().take(2);

            let route_name = self
                .gtfs
                .routes
                .get(&route_id)
                .and_then(|r| r.short_name.clone());
            let arrivals: Vec<TrainArrival> = next_trains
                .map(|&seconds| {
                    let future_time = ChronoUtc::now() + ChronoDuration::seconds(seconds as i64);
                    let human_time = chrono_humanize::HumanTime::from(future_time).to_string();
                    TrainArrival {
                        route_id: route_id.clone(),
                        route_name: route_name.clone(),
                        arrival_time: seconds,
                        human_time,
                    }
                })
                .collect();

            train_arrivals.insert(route_id, arrivals);
        }

        Ok(StopStatus {
            stop_id: stop_id.to_string(),
            stop_name: self.get_stop_name(stop_id),
            routes,
            train_arrivals,
        })
    }

    /// Gets the next N arrivals for a specific route at a stop
    pub async fn get_route_arrivals(
        &self,
        stop_id: &str,
        route_id: &str,
        _limit: usize,
    ) -> Result<Vec<TrainArrival>> {
        let status = self.get_stop_status(stop_id).await?;
        Ok(status
            .train_arrivals
            .get(route_id)
            .cloned()
            .unwrap_or_default())
    }

    /// Gets all upcoming arrivals at a stop, sorted by arrival time
    pub async fn get_all_arrivals(&self, stop_id: &str) -> Result<Vec<TrainArrival>> {
        let status = self.get_stop_status(stop_id).await?;
        let mut all_arrivals: Vec<TrainArrival> =
            status.train_arrivals.values().flatten().cloned().collect();

        all_arrivals.sort_by_key(|arrival| arrival.arrival_time);
        Ok(all_arrivals)
    }

    /// Prints GTFS statistics (useful for debugging)
    pub fn print_stats(&self) {
        self.gtfs.print_stats();
    }

    /// Formats a stop for display as "Name (Direction)"
    pub fn format_stop_display(&self, stop_id: &str, stop_name: &str) -> String {
        return format!("{} ({})", stop_name, stop_id);
    }
}

/// Configuration for the train checker
#[derive(Debug, Clone)]
pub struct TrainCheckerConfig {
    pub update_interval: Duration,
    pub max_arrivals_per_route: usize,
}

impl Default for TrainCheckerConfig {
    fn default() -> Self {
        Self {
            update_interval: Duration::from_secs(10),
            max_arrivals_per_route: 2,
        }
    }
}

/// A monitor that continuously updates stop status
pub struct StopMonitor {
    checker: TrainChecker,
    config: TrainCheckerConfig,
}

impl StopMonitor {
    /// Creates a new stop monitor
    pub async fn new(config: TrainCheckerConfig) -> Result<Self> {
        let checker = TrainChecker::new().await?;
        Ok(Self { checker, config })
    }

    /// Monitors a stop continuously, calling the callback with updates
    pub async fn monitor_stop<F>(&self, stop_id: &str, mut callback: F) -> Result<()>
    where
        F: FnMut(StopStatus) + Send + 'static,
    {
        loop {
            match self.checker.get_stop_status(stop_id).await {
                Ok(status) => callback(status),
                Err(e) => eprintln!("Error getting stop status: {}", e),
            }

            tokio::time::sleep(self.config.update_interval).await;
        }
    }
}
