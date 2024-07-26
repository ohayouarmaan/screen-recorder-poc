use crabgrab::prelude::{CapturableContent, CapturableContentFilter, WgpuVideoFrameError};
use tokio::sync::mpsc::UnboundedReceiver;
use rand::Rng;
use tokio::sync::mpsc;
use std::sync::Arc;
use std::{iter, num::NonZeroU32};
use crabgrab::capture_stream::{CaptureConfig, CapturePixelFormat, CaptureStream, StreamEvent};
use crabgrab::feature::wgpu::{
    WgpuCaptureConfigExt, WgpuVideoFrameExt, WgpuVideoFramePlaneTexture,
};
use wgpu::Texture;
use crabgrab::frame::VideoFrame;
use crabgrab::util::Size;
#[cfg(target_os = "windows")]
use crabgrab::platform::windows::WindowsCaptureConfigExt;

// Create a random texture
fn create_random_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let texture_extent = wgpu::Extent3d {
        width: 1024,
        height: 1024,
        depth_or_array_layers: 1,
    };

    let texture_desc = wgpu::TextureDescriptor {
        label: Some("Random Texture"),
        size: texture_extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[]
    };

    let texture = device.create_texture(&texture_desc);

    let mut rng = rand::thread_rng();
    let random_data: Vec<u8> = iter::repeat_with(|| rng.gen::<u8>())
        .take((texture_extent.width * texture_extent.height * 4) as usize)
        .collect();

    let texture_data = wgpu::ImageCopyTexture {
        texture: &texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    };

    let buffer_layout = wgpu::ImageDataLayout {
        offset: 0,
        bytes_per_row: Some(4 * texture_extent.width),
        rows_per_image: Some(texture_extent.height),
    };

    queue.write_texture(texture_data, &random_data, buffer_layout, texture_extent);

    texture
}

// Function to get the frame texture into a buffer
async fn get_frame_texture_to_buffer(
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    texture: &Texture,
) -> Vec<u8> {
    let texture_extent = wgpu::Extent3d {
        width: 1024,
        height: 1024,
        depth_or_array_layers: 1,
    };

    let buffer_size = (texture.width() * texture.height() * 4) as u64; // Assuming 4 bytes per pixel (RGBA)
    let buffer_desc = wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    };

    let buffer = device.create_buffer(&buffer_desc);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Copy Encoder"),
    });

    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * texture.width()),
                rows_per_image: Some(texture.height()),
            },
        },
        texture.size(),
    );

    queue.submit(Some(encoder.finish()));

    let buffer_slice = buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).unwrap();
    });

    device.poll(wgpu::Maintain::Wait);

    if let Ok(Ok(())) = receiver.await {
        let data = buffer_slice.get_mapped_range();
        let result = data.to_vec();
        drop(data);
        buffer.unmap();
        result
    } else {
        vec![]
    }
}

// Main function
#[tokio::main]
async fn main() {
    println!("MAIN RUNNING");
    let (device, queue) = initialize_wgpu().await;

    let device = Arc::new(device);
    let queue = Arc::new(queue);
    
    let (tx, mut rx) = mpsc::unbounded_channel::<VideoFrame>();
    match CaptureStream::test_access(false) {
        Some(token) => {
            let filter = CapturableContentFilter::DISPLAYS;
            let content = CapturableContent::new(filter).await.unwrap();
            struct WgpuDeviceWrapper(Arc<wgpu::Device>);
            impl AsRef<wgpu::Device> for WgpuDeviceWrapper {
                fn as_ref(&self) -> &wgpu::Device {
                    &self.0
                }
            }
            let w: Arc<dyn AsRef<wgpu::Device> + Send + Sync> = Arc::new(WgpuDeviceWrapper(device.clone()));
            let mut displays = content.displays();
            let config = CaptureConfig::with_display(displays.next().unwrap(), CapturePixelFormat::Bgra8888).with_wgpu_device(w);
            let stream = CaptureStream::new(token, config.unwrap(), move |result| match result {
                Ok(event) => match event {
                    StreamEvent::Video(frame) => {
                        tx.send(frame).expect("Failed to send frame")
                    }
                    _ => {}
                },
                Err(e) => println!("Error: {}", e),
            });
            while let Some(frame) = rx.recv().await {
                let texture: Result<Texture, WgpuVideoFrameError> = frame.get_wgpu_texture(WgpuVideoFramePlaneTexture::Rgba, Some("POC WGPU Texture"));
                let wgpu_dev = device.clone();
                let wgpu_queue = queue.clone();
                println!("sending data to another async thread");
                let buffer_data = get_frame_texture_to_buffer(wgpu_dev.clone(), wgpu_queue.clone(), &(texture.unwrap())).await;
                println!("Buffer Data: {:?}", &buffer_data[..10]);
            }
        },
        None => {
            panic!("No access")
        }
    }

}

// Helper function to initialize wgpu
async fn initialize_wgpu() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        ..Default::default()
    });
    let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions::default()).await.unwrap();
    println!("WGPU Initialized");
    adapter.request_device(&wgpu::DeviceDescriptor::default(), None).await.unwrap()
}
