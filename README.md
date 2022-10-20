## Youtube Party Bot


This telegram bot is intended to be played on a remote computer, connected to a
screen and audio output.

Upon adding them to a group, users there can share youtube links which will
then be played sequentially using `mpv`.

Those in the group are allowed to add, but not skip videos.

The maintainer can call the following commands:

- `/next`, will stop the current video and directly go to the next in the queue

Everyone can use the following commands:

- `/help`, to get a list of available commands
- `/queue`, to send a message containing all to be played videos.


## How does it work?

The system uses `mpv` and `yt-dlp` to play videos in fullscreen.
The bot should be added to a _single_ group which will serve as _the control_
group of the bot. There users can send commands to the bot. Only people you
__trust__ should be added.

## Running the bot

The bot requires three parts:

- `TELOXIDE_TOKEN` env variable in which you paste your telegram bot token
- `--maintainer` argument which is the user id of the telegram account that
  gets to control the bot directly
- `--group` argument that signifies the group in which the bot was added


**Using Nix flakes**

```bash
❯ nix run github:TheNeikos/hemerashomepartybot -- --maintainer=29292292 --group=-828282828
```

**Running manually**

Check out the repository and install `mpv`/`yt-dlp`, then run:

```bash
❯ cargo run -- --maintainer=29292292 --group=-828282828
```

