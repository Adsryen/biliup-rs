use crate::client::StatelessClient;
use crate::error::Kind;
use crate::uploader::bilibili::{BiliBili, Studio, Vid, Video};
use crate::uploader::credential::login_by_cookies;
use crate::uploader::line::Line;
use crate::uploader::VideoFile;
use futures::StreamExt;
use std::path::{Path, PathBuf};

use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info};

struct UploadActor {
    receiver: mpsc::UnboundedReceiver<ActorMessage>,
    client: StatelessClient,
    vid: Option<Vid>,
}
enum ActorMessage {
    Upload { path: PathBuf },
}

impl UploadActor {
    fn new(client: StatelessClient, receiver: mpsc::UnboundedReceiver<ActorMessage>) -> Self {
        UploadActor {
            receiver,
            client,
            vid: None,
        }
    }

    async fn upload(
        &self,
        video_paths: &[PathBuf],
        bili: &BiliBili,
        line: Line,
        limit: usize,
    ) -> crate::error::Result<Vec<Video>> {
        let mut videos = Vec::new();
        for video_path in video_paths {
            println!("{:?}", video_path.canonicalize()?.to_str());
            info!("{line:?}");
            let video_file = VideoFile::new(video_path)?;
            let total_size = video_file.total_size;
            let file_name = video_file.file_name.clone();
            let uploader = line.pre_upload(bili, video_file).await?;

            let instant = Instant::now();

            let video = uploader
                .upload(self.client.clone(), limit, |vs| {
                    vs.map(|vs| {
                        let chunk = vs?;
                        let len = chunk.len();
                        Ok((chunk, len))
                    })
                })
                .await?;
            let t = instant.elapsed().as_millis();
            info!(
                "Upload completed: {file_name} => cost {:.2}s, {:.2} MB/s.",
                t as f64 / 1000.,
                total_size as f64 / 1000. / t as f64
            );
            videos.push(video);
        }
        Ok(videos)
    }

    async fn handle_message(&mut self, msg: ActorMessage) -> crate::error::Result<()> {
        match msg {
            ActorMessage::Upload { path } => {
                let bili = login_by_cookies("cookies.json").await?;
                let videos = self.upload(&[path], &bili, Default::default(), 3).await?;

                if let Some(vid) = &self.vid {
                    let mut studio = bili.studio_data(vid).await?;
                    studio.videos.extend(videos);
                    bili.edit(&studio).await?;
                } else {
                    let studio = Studio::builder()
                        .desc("desc".to_string())
                        .tag("tag".to_string())
                        .title("test")
                        .videos(videos)
                        .build();

                    let result = bili.submit(&studio).await?;
                    self.vid = Some(
                        result
                            .data
                            .as_ref()
                            .and_then(|data| data.get("bvid"))
                            .and_then(|vid| vid.as_str())
                            .map(|vid| Vid::Bvid(String::from(vid)))
                            .ok_or_else(|| Kind::Custom(format!("{:?}", result)))?,
                    );
                }
            }
        }
        Ok(())
    }
}

async fn run_download_actor(mut actor: UploadActor) {
    while let Some(msg) = actor.receiver.recv().await {
        match actor.handle_message(msg).await {
            Ok(_) => {}
            Err(e) => {
                error!("{}", e)
            }
        };
    }
}

#[derive(Clone)]
pub struct UploadActorHandle {
    sender: mpsc::UnboundedSender<ActorMessage>,
}

impl UploadActorHandle {
    pub fn new(client: StatelessClient) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let actor = UploadActor::new(client, receiver);
        tokio::spawn(run_download_actor(actor));

        Self { sender }
    }

    pub fn send_file_path<T: AsRef<Path>>(&self, path: T) {
        let msg = ActorMessage::Upload {
            path: PathBuf::from(path.as_ref()),
        };
        let _ = self.sender.send(msg);
    }
}
