//! Node-API surface used by `spikes/osr-host/src/main.ts`.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use napi::bindgen_prelude::{AsyncTask, Buffer};
use napi::{Env, Error, Result, Task};
use napi_derive::napi;

use crate::{Bitmap, FrameInfo, PlaneFd, SendResult, Server};

const ACK_TIMEOUT: Duration = Duration::from_millis(100);

static SERVER: OnceLock<Server> = OnceLock::new();

#[napi(object)]
pub struct JsPlane {
    pub fd: i32,
    pub stride: u32,
    pub offset: u32,
    pub size: u32,
}

#[napi(object)]
pub struct JsFrame {
    pub widget: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub modifier: String,
    pub planes: Vec<JsPlane>,
}

#[napi]
pub fn start_server(socket_path: String) -> Result<()> {
    let server = Server::bind(Path::new(&socket_path)).map_err(|e| Error::from_reason(e.to_string()))?;
    SERVER.set(server).map_err(|_| Error::from_reason("server already started"))
}

/// Changes whenever a new layer connects; the host repaints all widgets when it does.
#[napi]
pub fn peer_generation() -> u32 {
    SERVER.get().map_or(0, |s| s.peer_generation() as u32)
}

pub struct SendFrameTask(FrameInfo);

impl Task for SendFrameTask {
    type Output = SendResult;
    type JsValue = String;

    fn compute(&mut self) -> Result<SendResult> {
        Ok(SERVER.get().map_or(SendResult::Dropped, |s| s.send_frame(&self.0, ACK_TIMEOUT)))
    }

    fn resolve(&mut self, _env: Env, output: SendResult) -> Result<String> {
        Ok(output.as_str().to_owned())
    }
}

/// Resolves to `'acked'`, `'dropped'` (no layer connected) or `'timeout'` (no ack in 100 ms).
#[napi(ts_return_type = "Promise<'acked' | 'dropped' | 'timeout'>")]
pub fn send_frame(f: JsFrame) -> AsyncTask<SendFrameTask> {
    AsyncTask::new(SendFrameTask(FrameInfo {
        widget: f.widget,
        x: f.x,
        y: f.y,
        width: f.width,
        height: f.height,
        pixel_format: f.pixel_format,
        modifier: f.modifier,
        planes: f
            .planes
            .into_iter()
            .map(|p| PlaneFd { fd: p.fd, stride: p.stride, offset: p.offset, size: p.size })
            .collect(),
    }))
}

#[napi(object)]
pub struct JsBitmap {
    pub widget: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: String,
    pub data: Buffer,
}

/// Holds the JS bitmap buffer alive until the worker thread has copied it into a slot.
pub struct SendBitmapTask(JsBitmap);

impl Task for SendBitmapTask {
    type Output = SendResult;
    type JsValue = String;

    fn compute(&mut self) -> Result<SendResult> {
        let b = &self.0;
        let bitmap = Bitmap {
            widget: b.widget,
            x: b.x,
            y: b.y,
            width: b.width,
            height: b.height,
            stride: b.stride,
            pixel_format: &b.pixel_format,
            data: &b.data,
        };
        Ok(SERVER.get().map_or(SendResult::Dropped, |s| s.send_bitmap(&bitmap, ACK_TIMEOUT)))
    }

    fn resolve(&mut self, _env: Env, output: SendResult) -> Result<String> {
        Ok(output.as_str().to_owned())
    }
}

/// CPU-rendered widget frame → shared-memory slot → layer. Same results as `sendFrame`.
#[napi(ts_return_type = "Promise<'acked' | 'dropped' | 'timeout'>")]
pub fn send_bitmap(b: JsBitmap) -> AsyncTask<SendBitmapTask> {
    AsyncTask::new(SendBitmapTask(b))
}
