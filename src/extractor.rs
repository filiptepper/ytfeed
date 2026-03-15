use crate::{error::Error, feed::Channel};
use reqwest::{Client, StatusCode};
use scraper::{Html, Selector};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub id: String,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct Extraction {
    pub channel: Channel,
    pub videos: Vec<VideoInfo>,
}

/// Extracts channel data and video information by scraping the YouTube website
pub async fn extract_data(id_or_handle: &str, client: &Client) -> Result<Extraction, Error> {
    let videos_url = if id_or_handle.starts_with("UC") {
        format!("https://www.youtube.com/channel/{}/videos", id_or_handle)
    } else {
        format!("https://www.youtube.com/@{}/videos", id_or_handle)
    };
    tracing::debug!("scraping channel data from '{}'", videos_url);
    let response = client
        .get(&videos_url)
        .header("Accept-Language", "en") // to get data in English locale formats
        .send()
        .await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(Error::ChannelNotFound(id_or_handle.to_string()));
    }
    let text = response.error_for_status()?.text().await?;
    let html = Html::parse_fragment(&text);
    let script_selector = Selector::parse("script").unwrap();
    for element in html.select(&script_selector) {
        let script = element.inner_html();
        let script = script.trim();
        if !script.starts_with("var ytInitialData") {
            continue;
        }
        let json = script
            .strip_prefix("var ytInitialData = ")
            .ok_or_else(|| Error::Scrape("failed to strip prefix"))?
            .strip_suffix(';')
            .ok_or_else(|| Error::Scrape("failed to strip suffix"))?;
        let data: Value = serde_json::from_str(json)?;
        let meta_data = &data["metadata"]["channelMetadataRenderer"];
        let channel_id = meta_data["externalId"].as_str().unwrap().to_string();
        let channel = Channel {
            title: meta_data["title"].as_str().unwrap().to_string(),
            url: format!("https://www.youtube.com/channel/{channel_id}"),
            id: channel_id,
        };

        let videos_tab_selector = |tab: &Value| {
            tab["tabRenderer"]["endpoint"]["commandMetadata"]["webCommandMetadata"]["url"]
                .as_str()
                .map(|url| url.ends_with("/videos"))
                .unwrap_or(false)
        };

        let tabs = data["contents"]["twoColumnBrowseResultsRenderer"]["tabs"]
            .as_array()
            .ok_or_else(|| Error::Scrape("failed to find tabs array"))?;

        let video_tab = tabs
            .iter()
            .find(|t| videos_tab_selector(t))
            .or_else(|| tabs.get(1)) // Fallback to index 1 (usually Videos)
            .or_else(|| tabs.get(0)) // Fallback to index 0
            .ok_or_else(|| Error::Scrape("failed to find any tab"))?;

        let videos_parent = &video_tab["tabRenderer"]["content"]["richGridRenderer"]["contents"];
        let videos_array = if let Some(arr) = videos_parent.as_array() {
            arr
        } else {
            // Try another common location for videos
            let alt_parent = &video_tab["tabRenderer"]["content"]["sectionListRenderer"]["contents"][0]
                ["itemSectionRenderer"]["contents"][0]["gridRenderer"]["items"];
            alt_parent
                .as_array()
                .ok_or_else(|| Error::Scrape("failed to find videos array in both locations"))?
        };

        let mut videos = Vec::new();
        for item in videos_array {
            let video_renderer = item
                .get("richItemRenderer")
                .and_then(|r| r["content"].get("videoRenderer"))
                .or_else(|| item.get("videoRenderer"));

            if let Some(video_renderer) = video_renderer {
                let id = video_renderer["videoId"]
                    .as_str()
                    .ok_or_else(|| Error::Scrape("failed to find video id"))?
                    .to_string();
                let length_text = video_renderer["lengthText"]["simpleText"]
                    .as_str()
                    .ok_or_else(|| Error::Scrape("failed to find length text"))?;
                let parts: Vec<&str> = length_text.split(':').collect();
                let duration = if parts.len() == 3 {
                    let hours: u64 = parts[0].parse().map_err(|_| Error::Scrape("invalid hours"))?;
                    let minutes: u64 = parts[1]
                        .parse()
                        .map_err(|_| Error::Scrape("invalid minutes"))?;
                    let seconds: u64 = parts[2]
                        .parse()
                        .map_err(|_| Error::Scrape("invalid seconds"))?;
                    Duration::from_secs(hours * 3600 + minutes * 60 + seconds)
                } else if parts.len() == 2 {
                    let minutes: u64 = parts[0]
                        .parse()
                        .map_err(|_| Error::Scrape("invalid minutes"))?;
                    let seconds: u64 = parts[1]
                        .parse()
                        .map_err(|_| Error::Scrape("invalid seconds"))?;
                    Duration::from_secs(minutes * 60 + seconds)
                } else {
                    return Err(Error::Scrape("invalid number of parts in length text"));
                };
                let video = VideoInfo { id, duration };
                videos.push(video);
            }
        }
        tracing::debug!("scraped {} videos from '{}'", videos.len(), channel.title);
        return Ok(Extraction { channel, videos });
    }
    Err(Error::ChannelNotFound(id_or_handle.to_string()))
}
