use std::{collections::VecDeque, fmt::Display, process::Stdio, sync::Arc, time::Duration};

use clap::Parser;
use regex::Regex;
use teloxide::{
    dispatching::{HandlerExt, UpdateFilterExt},
    dptree,
    payloads::SendMessageSetters,
    prelude::{Dispatcher, Requester, ResponseResult},
    types::{ChatId, InputFile, Message, MessageEntityKind, Update, User, UserId},
    utils::command::BotCommands,
    Bot,
};
use tokio::{process::Command, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    maintainer: u64,
    #[arg(short, long)]
    group: i64,
}

#[tokio::main]
async fn main() {
    let fmt_layer = tracing_subscriber::fmt::layer();
    let filter_layer = EnvFilter::from_default_env();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    let bot = Bot::from_env();
    let args = Args::parse();

    info!("Started bot!");

    let params = ConfigParameters {
        bot_maintainer: UserId(args.maintainer),
        authorized_group: ChatId(args.group),
    };

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
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
                        Ok(())
                    }),
                ),
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
                    yt_regex
                        .captures(entity.text())
                        .map(|yt_matches| yt_matches.get(1).unwrap().as_str().to_string())
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
            queue
                .add_youtube_to_queue(
                    id,
                    msg.from()
                        .cloned()
                        .expect("Did not receive a message from a user?"),
                )
                .await;
        }

        queue.start_playing_if_empty().await;
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct YoutubeVideo {
    title: String,
    length: usize,
    id: String,
}

#[derive(Debug, Clone)]
enum MediumKind {
    Youtube(YoutubeVideo),
}

#[derive(Debug, Clone)]
struct Medium {
    adder: User,
    kind: MediumKind,
}

impl Display for MediumKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediumKind::Youtube(yt) => write!(
                f,
                "🎬 [{title}](https://youtube.com/watch?v={id}) _{length}_",
                id = yt.id,
                title = yt.title,
                length = humantime::format_duration(Duration::from_secs(yt.length as u64)),
            ),
        }
    }
}

struct MediaQueue {
    media: Arc<Mutex<VecDeque<Medium>>>,
    current_player: Mutex<Option<CancellationToken>>,
    bot: Bot,
    cfg: ConfigParameters,
    client: invidious::reqwest::asynchronous::Client,
}

impl MediaQueue {
    fn new(bot: Bot, cfg: ConfigParameters) -> Self {
        Self {
            media: Default::default(),
            current_player: Default::default(),
            bot,
            cfg,
            client: invidious::reqwest::asynchronous::Client::new(String::from(
                "https://vid.puffyan.us",
            )),
        }
    }
}

impl MediaQueue {
    pub async fn get_current_queue(&self) -> VecDeque<Medium> {
        self.media.lock().await.clone()
    }
    pub async fn next_video(&self) {
        debug!("Skipped to next id, continuing!");
        self.start_playing().await;
    }
    pub async fn add_youtube_to_queue(&self, id: String, adder: User) {
        {
            let mut q = self.media.lock().await;
            debug!(?id, "Added to queue");

            let info = self.client.video(&id, None).await;

            let (title, length) = {
                if let Ok(info) = info {
                    (info.title, info.length as usize)
                } else {
                    (String::from("Unknown"), 0)
                }
            };

            let vid = Medium {
                adder,
                kind: MediumKind::Youtube(YoutubeVideo { id, title, length }),
            };
            q.push_back(vid);
        }
    }

    pub async fn start_playing_if_empty(&self) {
        if self.current_player.lock().await.is_none() {
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
        let bot = self.bot.clone();
        let cfg = self.cfg.clone();

        debug!("Spawning new player future");
        let token = CancellationToken::new();
        *ply = Some(token.clone());
        tokio::spawn(async move {
            loop {
                let vid: Option<Medium> = {
                    let mut q = media.lock().await;
                    q.pop_front()
                };

                if let Some(vid) = vid {
                    let _ = bot
                        .send_message(cfg.authorized_group, format!("Now playing {}", vid.kind))
                        .parse_mode(
                            #[allow(deprecated)]
                            teloxide::types::ParseMode::Markdown,
                        )
                        .await;

                    match vid.kind {
                        MediumKind::Youtube(yt_vid) => spawn_download(yt_vid.id, &token).await,
                    }

                    if token.is_cancelled() {
                        debug!("Token got cancelled, stopping loop");
                        break;
                    }
                    debug!("Media finished");
                } else {
                    debug!("No more videos in queue, stopping player.");
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
            let currently_playing = { queue.current_player.lock().await.is_some() };
            let mut answer = String::from("The current queue:\n\n");

            for (idx, elem) in media_queue.iter().enumerate() {
                answer.push_str(&format!(
                    "{prefix} {title} \n  *Added By:* {adder}\n",
                    prefix = if idx == 0 { "🔜" } else { "➡️" },
                    title = elem.kind,
                    adder = elem.adder.full_name(),
                ));
            }

            answer.push_str(&format!(
                "\n*Status:* {}",
                if currently_playing {
                    "Playing"
                } else {
                    "Not Playing"
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
