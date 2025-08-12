import 'dotenv/config';
import { Client, GatewayIntentBits, REST, Routes, SlashCommandBuilder, ChannelType } from 'discord.js';
import { joinVoiceChannel, createAudioPlayer, createAudioResource, AudioPlayerStatus, VoiceConnectionStatus, entersState, NoSubscriberBehavior, StreamType } from '@discordjs/voice';
import { Readable } from 'node:stream';
import WebSocket from 'ws';
import fetch from 'node-fetch';

const TOKEN = process.env.DISCORD_TOKEN;
const CLIENT_ID = process.env.CLIENT_ID;
const GUILD_ID = process.env.GUILD_ID;
const RESONIX_BASE = process.env.RESONIX_BASE || 'http://localhost:2333';

// --- Commands ---
const commands = [
  new SlashCommandBuilder().setName('join').setDescription('Join your voice channel'),
  new SlashCommandBuilder().setName('leave').setDescription('Leave the voice channel'),
  new SlashCommandBuilder().setName('play').setDescription('Play a URL via Resonix').addStringOption(o=>o.setName('url').setDescription('Direct media URL').setRequired(true)),
  new SlashCommandBuilder().setName('pause').setDescription('Pause playback'),
  new SlashCommandBuilder().setName('resume').setDescription('Resume playback'),
  new SlashCommandBuilder().setName('volume').setDescription('Set volume (0.0-5.0)').addNumberOption(o=>o.setName('value').setDescription('Volume level (0.0-5.0)').setRequired(true)),
].map(c=>c.toJSON());

async function registerCommands() {
  const rest = new REST({ version: '10' }).setToken(TOKEN);
  if (GUILD_ID) {
    await rest.put(Routes.applicationGuildCommands(CLIENT_ID, GUILD_ID), { body: commands });
  } else {
    await rest.put(Routes.applicationCommands(CLIENT_ID), { body: commands });
  }
}

// PCM frame stream (S16LE stereo 48k, 20ms chunks = 3840 bytes/frame).
class PcmFrameStream extends Readable {
  constructor() { super({ read(){} }); }
  pushPacket(buf) { this.push(buf); }
  endStream() { this.push(null); }
}

// --- Bot state per guild ---
const state = new Map(); // guildId -> { connection, player, ws, stream }

const client = new Client({ intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildVoiceStates] });

client.once('ready', async () => {
  console.log(`Logged in as ${client.user.tag}`);
  try { await registerCommands(); console.log('Commands registered'); } catch (e) { console.error('Slash registration failed', e); }
});

client.on('interactionCreate', async (itx) => {
  if (!itx.isChatInputCommand()) return;
  const gid = itx.guildId;
  await itx.deferReply({ ephemeral: true });
  switch (itx.commandName) {
    case 'join': {
      const me = itx.member;
      const ch = me?.voice?.channel;
      if (!ch || ch.type !== ChannelType.GuildVoice) return void itx.editReply({ content: 'Join a voice channel first.', ephemeral: true });
      const conn = joinVoiceChannel({ channelId: ch.id, guildId: ch.guild.id, adapterCreator: ch.guild.voiceAdapterCreator, selfDeaf: true });
      try { await entersState(conn, VoiceConnectionStatus.Ready, 15_000); } catch (e) { conn.destroy(); return void itx.editReply({ content: 'Failed to join', ephemeral: true }); }
      const player = createAudioPlayer({ behaviors: { noSubscriber: NoSubscriberBehavior.Play } });
      conn.subscribe(player);
      state.set(gid, { connection: conn, player, ws: null, stream: null });
      return void itx.editReply({ content: 'Joined.', ephemeral: true });
    }
    case 'leave': {
      const s = state.get(gid);
      if (s) {
        s.ws?.close();
        s.connection?.destroy();
        try { await fetch(`${RESONIX_BASE}/players/g${gid}`, { method: 'DELETE' }); } catch {}
      }
      state.delete(gid);
      return void itx.editReply({ content: 'Left.', ephemeral: true });
    }
    case 'play': {
      const url = itx.options.getString('url', true);
      const s = state.get(gid);
      if (!s?.connection) return void itx.editReply({ content: 'Use /join first.', ephemeral: true });

      const playerId = `g${gid}`;

      // Cleanup
      s.ws?.close(); s.ws = null;
      s.stream?.endStream(); s.stream = null;
      s.player.stop();
      try { await fetch(`${RESONIX_BASE}/players/${playerId}`, { method: 'DELETE' }); } catch {}

      // Create & start server-side decoder
      try {
        await fetch(`${RESONIX_BASE}/players`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id: playerId, uri: url }) });
        await fetch(`${RESONIX_BASE}/players/${playerId}/play`, { method: 'POST' });
      } catch (e) {
        console.error(e);
        return void itx.editReply({ content: 'Resonix create/play failed.', ephemeral: true });
      }

      // Connect WS and stream PCM to Discord
      const wsUrl = `${RESONIX_BASE.replace('http://', 'ws://').replace('https://', 'wss://')}/players/${playerId}/ws`;
      const ws = new WebSocket(wsUrl);
      const stream = new PcmFrameStream();
      // Raw PCM S16LE stereo 48k
      const resource = createAudioResource(stream, { inputType: StreamType.Raw, inlineVolume: false });
      // Ensure connection is ready before starting
      try { await entersState(s.connection, VoiceConnectionStatus.Ready, 10_000); } catch {}
      s.player.play(resource);

      // Directly push each 20ms frame as it arrives (server is already paced)
      ws.binaryType = 'arraybuffer';
      ws.on('open', () => console.log('PCM WS open'));
      let pktCount = 0;
      ws.on('message', (data) => {
        const buf = Buffer.isBuffer(data) ? data : Buffer.from(data);
        if (pktCount < 10) console.log(`recv pcm #${pktCount + 1}: ${buf.length} bytes`);
        pktCount++;
        stream.pushPacket(buf);
      });
      ws.on('close', () => { console.log('PCM WS close'); stream.endStream(); });
      ws.on('error', (err) => { console.error('WS error', err); stream.endStream(); });

      s.ws = ws; s.stream = stream;

  s.player.on(AudioPlayerStatus.Playing, () => console.log('Playing'));
      s.player.on('error', (e) => console.error('Player error', e));

      return void itx.editReply(`Playing: ${url}`);
    }
    case 'pause': {
      const s = state.get(gid); if (!s) return void itx.editReply({ content: 'Nothing to pause.', ephemeral: true });
      try { await fetch(`${RESONIX_BASE}/players/g${gid}/pause`, { method: 'POST' }); s.player.pause(); } catch {}
      return void itx.editReply({ content: 'Paused.', ephemeral: true });
    }
    case 'resume': {
      const s = state.get(gid); if (!s) return void itx.editReply({ content: 'Nothing to resume.', ephemeral: true });
      try { await fetch(`${RESONIX_BASE}/players/g${gid}/play`, { method: 'POST' }); s.player.unpause(); } catch {}
      return void itx.editReply({ content: 'Resumed.', ephemeral: true });
    }
    case 'volume': {
      const v = itx.options.getNumber('value', true);
      try { await fetch(`${RESONIX_BASE}/players/g${gid}/filters`, { method: 'PATCH', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ volume: v }) }); } catch {}
      return void itx.editReply({ content: `Volume -> ${v}`, ephemeral: true });
    }
  }
});

client.login(TOKEN);