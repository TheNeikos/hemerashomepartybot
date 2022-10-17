use std::{
    fmt::Display,
    process::Stdio,
    sync::{atomic::AtomicUsize, Arc},
};

use regex::Regex;
use teloxide::{
    dispatching::{HandlerExt, UpdateFilterExt},
    dptree,
    payloads::SendMessageSetters,
    prelude::{Dispatcher, Requester, ResponseResult},
    types::{ChatId, InputFile, Message, MessageEntityKind, ReplyMarkup, Update, UserId},
    utils::command::BotCommands,
    Bot,
};
use tokio::{process::Command, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() {
    let fmt_layer = tracing_subscriber::fmt::layer();
    let filter_layer = EnvFilter::from_default_env();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    let bot = Bot::from_env();

    info!("Started bot!");

    let params = ConfigParameters {
        bot_maintainer: UserId(74897340),
        authorized_group: ChatId(-866400246),
    };

    let handler = Update::filter_message()
        .branch(
            dptree::filter(|cfg: ConfigParameters, msg: Message| -> bool {
                msg.from()
                    .map(|user| user.id == cfg.bot_maintainer)
                    .unwrap_or_default()
            })
            .filter_command::<MaintainerCommands>()
            .endpoint(answer_maintainers),
        )
        .branch(
            dptree::filter(|msg: Message, cfg: ConfigParameters| {
                (msg.chat.is_group() || msg.chat.is_supergroup())
                    && msg.chat.id == cfg.authorized_group
            })
            .filter_command::<UserCommands>()
            .endpoint(answer_users),
        )
        .branch(
            dptree::filter(|msg: Message, cfg: ConfigParameters| {
                (msg.chat.is_group() || msg.chat.is_supergroup())
                    && msg.chat.id == cfg.authorized_group
            })
            .endpoint(answer_group),
        )
        .branch(
            dptree::filter(|msg: Message, cfg: ConfigParameters| {
                msg.chat.is_private()
                    && msg
                        .from()
                        .map(|user| user.id != cfg.bot_maintainer)
                        .unwrap_or_default()
            })
            .endpoint(|bot: Bot, msg: Message| async move {
                bot.send_message(
                    msg.chat.id,
                    "I'm sorry, but you're not authorized to interact with this bot privately.",
                )
                .reply_to_message_id(msg.id)
                .await?;
                bot.send_sticker(
                    msg.chat.id,
                    InputFile::file_id(
                        "CAACAgIAAxkBAAMLY0vpn7zaM5lockhD1jzrkMujR0gAAu4BAAK813YEA5042ZQ7LZEqBA",
                    ),
                )
                .await?;
                return Ok(());
            }),
        );

    let queue = Arc::new(MediaQueue::new(bot.clone(), params.clone()));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![params, queue])
        //.enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

#[derive(Debug, Clone)]
struct ConfigParameters {
    bot_maintainer: UserId,
    authorized_group: ChatId,
}

#[derive(Clone, BotCommands)]
#[command(
    rename_rule = "lowercase",
    description = "You can use the following commands:"
)]
enum MaintainerCommands {
    #[command(description = "skip current video")]
    Next,
}

async fn answer_maintainers(cmd: MaintainerCommands, queue: Arc<MediaQueue>) -> ResponseResult<()> {
    match cmd {
        MaintainerCommands::Next => {
            queue.next_video().await;
        }
    }
    Ok(())
}

async fn answer_group(bot: Bot, msg: Message, queue: Arc<MediaQueue>) -> ResponseResult<()> {
    if let Some(entities) = msg.parse_entities() {
        let youtube_ids = entities
            .iter()
            .filter_map(|entity| {
                if entity.kind() == &MessageEntityKind::Url {
                    let yt_regex = Regex::new(r"(?:.be/|/watch\?v=)([\w/\-]+)").unwrap();
                    if let Some(yt_matches) = yt_regex.captures(entity.text()) {
                        Some(yt_matches.get(1).unwrap().as_str().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let count = youtube_ids.len();
        if count == 0 {
            return Ok(());
        }

        bot.send_message(
            msg.chat.id,
            format!(
                "Added {} video{} to queue!",
                count,
                if count == 1 { "" } else { "s" }
            ),
        )
        .reply_to_message_id(msg.id)
        .await?;

        for id in youtube_ids {
            info!(?id, "Found id, adding to queue");
            queue.add_youtube_to_queue(id).await;
        }

        queue.start_playing_if_empty().await;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct YoutubeVideo {
    id: String,
}

#[derive(Debug, Clone)]
enum Medium {
    Youtube(YoutubeVideo),
}

impl Display for Medium {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Medium::Youtube(yt) => write!(f, "🎬 https://youtube.com/watch?v={}", yt.id),
        }
    }
}

struct MediaQueue {
    media: Arc<Mutex<Vec<Medium>>>,
    current_medium: Arc<AtomicUsize>,
    current_player: Mutex<Option<CancellationToken>>,
    bot: Bot,
    cfg: ConfigParameters,
}

impl MediaQueue {
    fn new(bot: Bot, cfg: ConfigParameters) -> Self {
        Self {
            media: Default::default(),
            current_medium: Arc::new(AtomicUsize::from(usize::MAX)),
            current_player: Default::default(),
            bot,
            cfg,
        }
    }
}

impl MediaQueue {
    pub async fn get_current_queue(&self) -> Vec<Medium> {
        self.media.lock().await.clone()
    }
    pub async fn next_video(&self) {
        let next_id = self.current_medium.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |val| {
                if val == usize::MAX {
                    None
                } else {
                    Some(val.wrapping_add(1))
                }
            },
        );
        if let Ok(_) = next_id {
            debug!("Skipped to next id, continuing!");
            self.start_playing().await;
        }
    }
    pub async fn add_youtube_to_queue(&self, id: String) {
        {
            let mut q = self.media.lock().await;
            debug!(?id, "Added to queue");
            q.push(Medium::Youtube(YoutubeVideo { id }));
        }
    }

    pub async fn start_playing_if_empty(&self) {
        if self.current_medium.compare_exchange(
            usize::MAX,
            0,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) == Ok(usize::MAX)
        {
            self.start_playing().await;
            debug!("Nothing was playing, starting now!");
        }
    }

    pub async fn start_playing(&self) {
        let mut ply = self.current_player.lock().await;

        if let Some(cur_token) = ply.take() {
            cur_token.cancel();
        }

        let media = self.media.clone();
        let current_medium = self.current_medium.clone();
        let bot = self.bot.clone();
        let cfg = self.cfg.clone();

        debug!("Spawning new player future");
        let token = CancellationToken::new();
        *ply = Some(token.clone());
        tokio::spawn(async move {
            loop {
                let vid: Option<Medium> = {
                    let q = media.lock().await;
                    let next_id = current_medium.load(std::sync::atomic::Ordering::SeqCst);
                    if next_id != usize::MAX {
                        q.get(next_id).cloned()
                    } else {
                        break;
                    }
                };

                if let Some(vid) = vid {
                    let _ = bot
                        .send_message(cfg.authorized_group, format!("Now playing {}", vid))
                        .await;
                    let vid: Medium = vid.clone();

                    let vid = vid;
                    match vid {
                        Medium::Youtube(yt_vid) => spawn_download(yt_vid.id, &token).await,
                    }

                    if token.is_cancelled() {
                        debug!("Token got cancelled, stopping loop");
                        break;
                    }
                    let next_id = current_medium.fetch_update(
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                        |val| {
                            if val == usize::MAX {
                                None
                            } else {
                                Some(val.wrapping_add(1))
                            }
                        },
                    );
                    debug!("Media finished, next id is: {next_id:?}");
                } else {
                    debug!("No more videos in queue, stopping player.");
                    current_medium.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
                    token.cancel();
                    break;
                }
            }
        });
        debug!("Spawned new player!");
    }
}

async fn spawn_download(id: String, token: &CancellationToken) {
    let mut mpv = Command::new("mpv")
        .arg(&format!("https://youtube.com/watch?v={}", id))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Could not spawn mpv");

    tokio::select! {
        _ = mpv.wait() => (),
        _ = token.cancelled() => {
            debug!("Killing mpv");
            mpv.kill().await.expect("could not kill");
            debug!("Waiting for mpv");
            mpv.wait().await.expect("could not wait");
        }
    }
}

#[derive(Clone, BotCommands)]
#[command(
    rename_rule = "lowercase",
    description = "You can use the following commands:"
)]
enum UserCommands {
    #[command(description = "display this help.")]
    Help,
    #[command(description = "Show the current queue")]
    Queue,
}

async fn answer_users(
    bot: Bot,
    msg: Message,
    cmd: UserCommands,
    queue: Arc<MediaQueue>,
) -> ResponseResult<()> {
    match cmd {
        UserCommands::Help => {
            let help = UserCommands::descriptions().to_string();
            bot.send_message(msg.chat.id, help).await?;
        }
        UserCommands::Queue => {
            let media_queue = queue.get_current_queue().await;
            let current = queue
                .current_medium
                .load(std::sync::atomic::Ordering::SeqCst);
            let mut answer = String::from("The current queue:\n\n");

            for (idx, elem) in media_queue.iter().enumerate() {
                answer.push_str(&format!(
                    "{} {}\n",
                    if idx == current { ">" } else { "-" },
                    elem
                ));
            }

            answer.push_str(&format!(
                "*Status:* {}",
                if current == usize::MAX {
                    "Not Playing"
                } else {
                    "Playing"
                }
            ));

            bot.send_message(msg.chat.id, answer)
                .reply_to_message_id(msg.id)
                .disable_web_page_preview(true)
                .parse_mode(
                    #[allow(deprecated)]
                    teloxide::types::ParseMode::Markdown,
                )
                .await?;
        }
    }
    Ok(())
}
