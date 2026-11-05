//! The current System Media Transport Controls session, through WinRT.

use std::sync::Arc;
use std::time::Duration;

use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
    ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{IRandomAccessStream, IRandomAccessStreamReference};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::core::Interface;

use super::{COVER_SIDE, Control, Cover, Player, Status, Track};

fn err(e: windows::core::Error) -> String {
    format!("media session: {}", e.message())
}

/// Title, artist and album.
type TrackKey = (String, String, String);

pub struct Smtc {
    manager: Manager,
    /// The last cover, keyed by track, so it is decoded once per track.
    cover: Option<(TrackKey, Option<Arc<Cover>>)>,
}

impl Smtc {
    pub fn new() -> Result<Self, String> {
        let manager = Manager::RequestAsync().and_then(|op| op.join()).map_err(err)?;
        Ok(Self { manager, cover: None })
    }

    fn session(&self) -> Option<Session> {
        // No current session is reported as an error with a null result.
        self.manager.GetCurrentSession().ok()
    }
}

/// Now, in the 100 ns units since 1601 that WinRT `DateTime` uses.
fn now_ticks() -> i64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime)
}

fn ticks(t: i64) -> Duration {
    Duration::from_nanos(u64::try_from(t).unwrap_or(0).saturating_mul(100))
}

/// Decodes a thumbnail to RGBA, scaled so its longer side is at most [`COVER_SIDE`].
fn decode(thumb: &IRandomAccessStreamReference) -> windows::core::Result<Cover> {
    let stream = thumb.OpenReadAsync()?.join()?.cast::<IRandomAccessStream>()?;
    let (width, height, rgba) = decode_stream(&stream, COVER_SIDE)?;
    Ok(Cover { width, height, rgba })
}

/// Decodes an image to straight RGBA rows, scaled down so its longer side is at most `side`.
pub(crate) fn decode_stream(
    stream: &IRandomAccessStream,
    side: u32,
) -> windows::core::Result<(u32, u32, Vec<u8>)> {
    let (_, (w, h), rgba) = decode_sized(stream, side)?;
    Ok((w, h, rgba))
}

/// Source size, decoded size and RGBA pixels.
pub(crate) type Decoded = ((u32, u32), (u32, u32), Vec<u8>);

/// Like [`decode_stream`], also handing back the image's own size before scaling:
/// `(source, decoded, rgba)`.
pub(crate) fn decode_sized(stream: &IRandomAccessStream, side: u32) -> windows::core::Result<Decoded> {
    let decoder = BitmapDecoder::CreateAsync(stream)?.join()?;
    let (w, h) = (decoder.OrientedPixelWidth()?, decoder.OrientedPixelHeight()?);
    let scale = (side as f32 / w.max(h).max(1) as f32).min(1.0);
    let (sw, sh) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let transform = BitmapTransform::new()?;
    transform.SetScaledWidth(sw)?;
    transform.SetScaledHeight(sh)?;
    transform.SetInterpolationMode(BitmapInterpolationMode::Fant)?;
    let pixels = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Rgba8,
            BitmapAlphaMode::Straight,
            &transform,
            ExifOrientationMode::RespectExifOrientation,
            ColorManagementMode::ColorManageToSRgb,
        )?
        .join()?
        .DetachPixelData()?;
    Ok(((w, h), (sw, sh), pixels.to_vec()))
}

impl Player for Smtc {
    fn read(&mut self) -> Result<Option<Track>, String> {
        let Some(session) = self.session() else {
            return Ok(None);
        };
        let props = session
            .TryGetMediaPropertiesAsync()
            .and_then(|op| op.join())
            .map_err(err)?;
        let text = |r: windows::core::Result<windows::core::HSTRING>| {
            r.map(|s| s.to_string_lossy()).unwrap_or_default()
        };
        let (title, artist, album) = (
            text(props.Title()),
            text(props.Artist()),
            text(props.AlbumTitle()),
        );
        if title.is_empty() && artist.is_empty() {
            return Ok(None);
        }
        let status = match session.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()) {
            Ok(PlaybackStatus::Playing) => Status::Playing,
            Ok(PlaybackStatus::Paused) => Status::Paused,
            _ => Status::Stopped,
        };
        let (mut position, mut length) = (None, None);
        if let Ok(tl) = session.GetTimelineProperties() {
            let start = tl.StartTime().map(|t| t.Duration).unwrap_or(0);
            let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0);
            if end > start {
                length = Some(ticks(end - start));
                let mut at = tl.Position().map(|t| t.Duration).unwrap_or(0) - start;
                // Apps update the position now and then; move it along since that update.
                if status == Status::Playing
                    && let Ok(updated) = tl.LastUpdatedTime()
                    && updated.UniversalTime > 0
                {
                    at += (now_ticks() - updated.UniversalTime).max(0);
                }
                position = Some(ticks(at.clamp(0, end - start)));
            }
        }
        let key = (title.clone(), artist.clone(), album.clone());
        // A miss is retried: apps often set the thumbnail a moment after the title.
        let cover = match &self.cover {
            Some((k, Some(c))) if *k == key => Some(Arc::clone(c)),
            _ => {
                let c = props.Thumbnail().ok().and_then(|t| decode(&t).ok()).map(Arc::new);
                self.cover = Some((key, c.clone()));
                c
            }
        };
        Ok(Some(Track {
            title,
            artist,
            album,
            status,
            position,
            length,
            cover,
        }))
    }

    fn send(&mut self, control: Control) -> Result<(), String> {
        let Some(session) = self.session() else {
            return Ok(());
        };
        let op = match control {
            Control::Previous => session.TrySkipPreviousAsync(),
            Control::PlayPause => session.TryTogglePlayPauseAsync(),
            Control::Next => session.TrySkipNextAsync(),
        };
        op.and_then(|op| op.join()).map(|_| ()).map_err(err)
    }
}
