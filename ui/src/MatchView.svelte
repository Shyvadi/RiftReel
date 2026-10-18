<script>
  import { convertFileSrc, invoke } from '@tauri-apps/api/core';
  import { category, clock, kda, when, words } from './lib/format.js';
  import Icon from './lib/Icon.svelte';

  /** `backLabel`: what the way back is called (the page it goes back to). */
  let { id, onBack, backLabel = 'All matches' } = $props();

  let match = $state(null);
  let error = $state('');
  /** The <video> element, once there is one. */
  let video = $state(null);
  /** Its position, for the timeline's playhead. */
  let videoTime = $state(0);
  let videoError = $state('');
  /** The clip in the player, or null for the whole match. */
  let clip = $state(null);
  /** Where to start once the player has loaded the file `show` put in it. */
  let pendingStart = null;
  let clipError = $state('');
  /** The part of the recording to save as a clip (recording seconds), once marked. */
  let markIn = $state(null);
  let markOut = $state(null);
  /** What is being cut while ffmpeg works: a moment's index, or 'range'. */
  let saving = $state(null);
  /** Why the last cut failed, and which it was. */
  let saveError = $state(null);
  /** A clip waiting for its second "Recycle" click, and the last action's message. */
  let confirming = $state(null);
  let clipNotice = $state('');
  /** Why the last ★ on the match didn't take. */
  let favoriteError = $state('');

  $effect(() => {
    const wanted = id;
    match = null;
    error = '';
    videoError = '';
    videoTime = 0;
    clip = null;
    pendingStart = null;
    clipError = '';
    markIn = null;
    markOut = null;
    saving = null;
    saveError = null;
    confirming = null;
    clipNotice = '';
    favoriteError = '';
    invoke('get_match', { id: wanted })
      .then((value) => {
        if (wanted === id) match = value;
      })
      .catch((e) => (error = String(e)));
  });

  const duration = $derived(
    Math.max(1, match?.durationS ?? Math.max(0, ...(match?.markers ?? []).map((m) => m.gameTimeS + m.postS))),
  );
  // 8 units of margin each side, so a marker at the very end isn't clipped.
  const x = (seconds) => 8 + (Math.min(Math.max(seconds, 0), duration) / duration) * 984;
  const gridlines = $derived(Array.from({ length: Math.floor(duration / 300) + 1 }, (_, i) => i * 300));
  const sourceText = {
    end_of_game_stats: 'from the end-of-game stats',
    game_end_event: "from Riot's GameEnd event",
  };

  // The asset protocol streams files in ranged reads; they are never loaded whole.
  const videoSrc = $derived(match?.videoPath ? convertFileSrc(match.videoPath) : null);
  const clips = $derived(match?.clips ?? []);
  /** The clip that has recording time `seconds` (clips are cut from the recording, so
   *  they share its timeline from their start). */
  const clipAt = (seconds) => clips.find((c) => c.path && c.fromS <= seconds && seconds < c.toS) ?? null;
  /** Without the whole match (its recording deleted), the player starts on the first clip. */
  const shown = $derived(clip ?? (videoSrc ? null : (clips.find((c) => c.path) ?? null)));
  const playerSrc = $derived(shown ? convertFileSrc(shown.path) : videoSrc);
  const shownIndex = $derived(shown ? clips.findIndex((c) => c.file === shown.file) : -1);
  const markersById = $derived(new Map((match?.markers ?? []).map((m) => [m.id, m])));
  /** Game time at video time 0 (negative when recording began in the loading screen). */
  const offset = $derived.by(() => {
    const start = match?.recording?.gameTimeAtStartS;
    if (start != null) return start;
    const aligned = (match?.markers ?? []).find((m) => m.videoTimeS != null);
    return aligned ? aligned.gameTimeS - aligned.videoTimeS : null;
  });
  const playhead = $derived(playerSrc && offset != null ? (shown?.fromS ?? 0) + videoTime + offset : null);
  /** Recording time as game time, or as itself when the two can't be lined up. */
  const gameClock = (seconds) => clock(offset != null ? seconds + offset : seconds);
  /** Whether the player can show recording time `seconds`. */
  const playable = (seconds) => seconds != null && (videoSrc != null || clipAt(Math.max(0, seconds)) != null);
  /** The marked part, in order, if it's long enough for a clip. */
  const range = $derived.by(() => {
    if (markIn == null || markOut == null) return null;
    const [from, to] = markIn <= markOut ? [markIn, markOut] : [markOut, markIn];
    return to - from >= 1 ? { from, to } : null;
  });

  /** Where a marker's clip starts in the video: its pre-roll before the event. */
  const markerStart = (marker) => (marker.videoTimeS == null ? null : marker.videoTimeS - marker.preS);
  /** A moment's markers that are on the recording. */
  const placed = (moment) =>
    moment.markerIds.map((markerId) => markersById.get(markerId)).filter((m) => m?.videoTimeS != null);
  /** A moment starts with its earliest marker's pre-roll. */
  function momentStart(moment) {
    const starts = placed(moment).map(markerStart);
    return starts.length ? Math.min(...starts) : null;
  }
  /** …and ends with its latest marker's post-roll. */
  function momentEnd(moment) {
    const ends = placed(moment).map((m) => m.videoTimeS + m.postS);
    return ends.length ? Math.max(...ends) : null;
  }
  /** The frame its clip's thumbnail shows: the event the moment is named after. */
  function momentMain(moment) {
    const members = placed(moment);
    return (members.find((m) => m.label === moment.label) ?? members[0])?.videoTimeS ?? null;
  }
  /** The clip saved for a span, if there is one (it starts on the keyframe before). */
  const savedFor = (from, to) =>
    from == null || to == null
      ? null
      : (clips.find((c) => c.fromS <= from + 0.01 && c.fromS > from - 1.01 && Math.abs(c.toS - to) < 0.5) ??
        null);

  /** Plays from recording time `seconds`: in the whole match, or in the clip that has
   *  that time when only the clips are left. */
  function seek(seconds) {
    if (seconds == null) return;
    const at = Math.max(0, seconds);
    if (videoSrc) {
      show(null, at);
    } else {
      const found = clipAt(at);
      if (found) show(found, at - found.fromS);
    }
  }

  /** Puts a clip, or the whole match for null, in the player and plays it from `at`. */
  function show(next, at = 0) {
    if ((next?.file ?? null) === (shown?.file ?? null)) {
      if (video) {
        video.currentTime = at;
        video.play().catch(() => {});
      }
    } else {
      // The new file plays once it has loaded (`started`).
      clip = next;
      videoError = '';
      videoTime = at;
      pendingStart = at;
    }
    video?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }

  function started() {
    if (pendingStart == null) return;
    video.currentTime = pendingStart;
    pendingStart = null;
    video.play().catch(() => {});
  }

  /** Cuts recording seconds `from`–`to` into a clip (a stream copy, a second or two). */
  async function saveClip(key, from, to, label, thumbnailS) {
    if (saving != null || from == null || to == null) return;
    saving = key;
    saveError = null;
    try {
      const saved = await invoke('save_clip', { id, fromS: from, toS: to, label, thumbnailS });
      if (match.clips) match.clips.push(saved);
      else match.clips = [saved];
    } catch (e) {
      saveError = { key, text: String(e) };
    } finally {
      saving = null;
    }
  }

  const saveMoment = (moment, i) =>
    saveClip(i, momentStart(moment), momentEnd(moment), moment.label, momentMain(moment));

  /** Marks the player's position as one end of the clip to save. */
  function mark(end) {
    if (!video) return;
    const at = (shown?.fromS ?? 0) + video.currentTime;
    if (end === 'in') markIn = at;
    else markOut = at;
  }

  /** A favorite match keeps its full video: the cleanup never removes it. */
  async function toggleMatchFavorite() {
    const favorite = !match.favorite;
    favoriteError = '';
    try {
      await invoke('set_match_favorite', { id, favorite });
      match.favorite = favorite;
    } catch (e) {
      favoriteError = String(e);
    }
  }

  async function toggleFavorite(c) {
    const favorite = !c.favorite;
    clipError = '';
    try {
      await invoke('set_clip_favorite', { id, file: c.file, favorite });
      c.favorite = favorite;
    } catch (e) {
      clipError = String(e);
    }
  }

  async function reveal(c) {
    clipError = '';
    try {
      await invoke('reveal_clip', { id, file: c.file });
    } catch (e) {
      clipError = String(e);
    }
  }

  let noticeTimer;
  /** A line under the clips for a few seconds. */
  function notice(text) {
    clipNotice = text;
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => (clipNotice = ''), 5000);
  }

  async function copyClip(c) {
    clipError = '';
    try {
      await invoke('copy_clip', { id, file: c.file });
      notice('Copied: paste it in Explorer, Discord or a chat.');
    } catch (e) {
      clipError = String(e);
    }
  }

  async function exportClip(c) {
    clipError = '';
    try {
      const to = await invoke('export_clip', { id, file: c.file });
      if (to) notice(`Saved a copy: ${to}`);
    } catch (e) {
      clipError = String(e);
    }
  }

  /** Sends a clip to the Recycle Bin, on a second click. */
  async function recycleClip(c) {
    if (confirming !== c.file) {
      confirming = c.file;
      setTimeout(() => {
        if (confirming === c.file) confirming = null;
      }, 4000);
      return;
    }
    confirming = null;
    clipError = '';
    try {
      if (!(await invoke('delete_clip', { id, file: c.file }))) return;
      if (shown?.file === c.file) clip = null;
      match.clips = match.clips.filter((other) => other.file !== c.file);
      notice('Moved to the Recycle Bin.');
    } catch (e) {
      clipError = String(e);
    }
  }

  /** A click on the timeline's background: play from that game time. */
  function seekTimeline(event) {
    if (!playerSrc || offset == null) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const units = ((event.clientX - rect.left) / rect.width) * 1000;
    seek(((units - 8) / 984) * duration - offset);
  }

  /** Click and keyboard handlers for a timeline mark that seeks. */
  function seeker(start) {
    return {
      onclick: (event) => {
        event.stopPropagation();
        seek(start);
      },
      onkeydown: (event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          event.stopPropagation();
          seek(start);
        }
      },
    };
  }
</script>

<div class="topbar">
  <button type="button" class="btn ghost" onclick={onBack}><Icon name="back" /> {backLabel}</button>
</div>

{#if error}
  <p class="error">{error}</p>
{:else if !match}
  <p class="muted">Loading…</p>
{:else}
  <header class="card head" class:won={match.result === 'win'} class:lost={match.result === 'lose'}>
    <div class="who">
      <div class="title">
        <h1>{match.champion ?? 'Unknown champion'}</h1>
        {#if match.source !== 'recording'}
          <button
            type="button"
            class="icon-btn star"
            class:on={match.favorite}
            aria-pressed={!!match.favorite}
            aria-label="Favorite"
            title={match.favorite
              ? 'Favorite: the cleanup never removes this match'
              : 'Mark as favorite: the cleanup never removes favorites'}
            onclick={toggleMatchFavorite}><Icon name={match.favorite ? 'star-filled' : 'star'} size={26} /></button
          >
        {/if}
      </div>
      <div class="sub">
        {[
          match.queue ?? match.gameMode,
          match.source === 'recording' && 'recording',
          match.source !== 'recording' && match.startedWallMs && when(match.startedWallMs),
          match.source === 'outplayed' && 'from Outplayed',
        ]
          .filter(Boolean)
          .join(' · ')}
      </div>
      {#if match.description}<div class="muted small">{match.description}</div>{/if}
      {#if favoriteError}<div class="error small">{favoriteError}</div>{/if}
    </div>
    <div class="outcome">
      <span class="badge">
        {match.result === 'win' ? 'Victory' : match.result === 'lose' ? 'Defeat' : 'Result unknown'}
      </span>
      {#if match.resultSource}<div class="muted small">{sourceText[match.resultSource] ?? ''}</div>{/if}
    </div>
    <div class="stats">
      <div><b>{kda(match.kda)}</b><span>K / D / A</span></div>
      <div><b>{clock(match.durationS)}</b><span>Length</span></div>
      <div><b>{match.team ?? '–'}</b><span>Team</span></div>
      <div><b>{match.moments.length}</b><span>Moments</span></div>
    </div>
  </header>

  {#if playerSrc}
    {@const recording = match.recording}
    <section class="card player">
      {#if shown}
        <div class="playing">
          <span>
            <b>Clip {shownIndex + 1}</b> · <span class="label">{words(shown.label)}</span>
            <span class="muted">· {gameClock(shown.fromS)}–{gameClock(shown.toS)}</span>
          </span>
          {#if videoSrc}
            <button type="button" class="btn small" onclick={() => show(null, shown.fromS + videoTime)}
              >Back to the whole match</button
            >
          {/if}
        </div>
      {/if}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        src={playerSrc}
        controls
        preload="metadata"
        onloadedmetadata={started}
        ontimeupdate={() => (videoTime = video.currentTime)}
        onerror={() => (videoError = shown ? 'This clip could not be played.' : 'This video could not be played.')}
      ></video>
      {#if videoError}<p class="error">{videoError}</p>{/if}
      {#if videoSrc}
        <div class="trim" role="group" aria-label="Make a clip">
          <button type="button" class="btn" onclick={() => mark('in')}>Mark in</button>
          <span class="time">{markIn == null ? '–' : gameClock(markIn)}</span>
          <button type="button" class="btn" onclick={() => mark('out')}>Mark out</button>
          <span class="time">{markOut == null ? '–' : gameClock(markOut)}</span>
          {#if range}<span class="muted">{(range.to - range.from).toFixed(1)} s</span>{/if}
          <span class="grow"></span>
          {#if markIn != null || markOut != null}
            <button type="button" class="btn ghost" onclick={() => ((markIn = null), (markOut = null))}>Clear</button>
          {/if}
          <button
            type="button"
            class="btn primary"
            disabled={!range || saving != null}
            onclick={() => saveClip('range', range.from, range.to, 'clip', null)}
          >
            <Icon name="scissors" />
            {saving === 'range' ? 'Saving…' : 'Save clip'}
          </button>
        </div>
        {#if saveError?.key === 'range'}<p class="error small">{saveError.text}</p>{/if}
      {/if}
      {#if match.source === 'outplayed'}
        <div class="muted small info">Recorded by Outplayed: {match.recording?.path}</div>
      {:else if recording}
        <div class="muted small info">
          {recording.width}×{recording.height} · {recording.fps} fps · {recording.encoder}
          {#if recording.droppedFrames}· {recording.droppedFrames} dropped frames{/if}
          {#if recording.error}· the recording ended early ({recording.error}){/if}
        </div>
      {/if}
    </section>
  {/if}

  {#if clips.length}
    <section class="card">
      <h2>Clips <span class="count">{clips.length}</span></h2>
      {#if clipError}<p class="error small">{clipError}</p>{/if}
      {#if clipNotice}<p class="notice">{clipNotice}</p>{/if}
      <ul class="clips">
        {#each clips as c, i (c.file)}
          <li class:current={shown?.file === c.file}>
            <button
              type="button"
              class="thumb"
              disabled={!c.path}
              aria-label="Play clip {i + 1}, {words(c.label)}"
              onclick={() => show(c)}
            >
              {#if c.thumbnailPath}
                <img src={convertFileSrc(c.thumbnailPath)} alt="" loading="lazy" />
              {:else}
                <span class="nothumb">{c.path ? '' : 'File missing'}</span>
              {/if}
              {#if c.path}<span class="overlay"><Icon name="play" size={30} /></span>{/if}
              <span class="length">{Math.round(c.toS - c.fromS)} s</span>
            </button>
            <div class="clip-info">
              <span class="name {category(c.label)}"><i></i><span class="label">{words(c.label)}</span></span>
              <span class="muted num">{gameClock(c.fromS)}</span>
            </div>
            <div class="clip-actions">
              <button
                type="button"
                class="icon-btn"
                class:on={c.favorite}
                aria-pressed={c.favorite}
                aria-label="Favorite clip {i + 1}"
                title={c.favorite ? 'Favorite' : 'Mark as favorite'}
                onclick={() => toggleFavorite(c)}><Icon name={c.favorite ? 'star-filled' : 'star'} /></button
              >
              <button
                type="button"
                class="icon-btn"
                disabled={!c.path}
                title="Show in Explorer"
                aria-label="Show clip {i + 1} in Explorer"
                onclick={() => reveal(c)}><Icon name="folder" /></button
              >
              <button
                type="button"
                class="icon-btn"
                disabled={!c.path}
                title="Copy (paste it in Explorer, Discord or a chat)"
                aria-label="Copy clip {i + 1}"
                onclick={() => copyClip(c)}><Icon name="copy" /></button
              >
              <button
                type="button"
                class="icon-btn"
                disabled={!c.path}
                title="Save a copy as…"
                aria-label="Save a copy of clip {i + 1}"
                onclick={() => exportClip(c)}><Icon name="download" /></button
              >
              {#if confirming === c.file}
                <button
                  type="button"
                  class="btn small danger recycle"
                  aria-label="Confirm: recycle clip {i + 1}"
                  onclick={() => recycleClip(c)}>Recycle?</button
                >
              {:else}
                <button
                  type="button"
                  class="icon-btn recycle"
                  title="Move to the Recycle Bin"
                  aria-label="Recycle clip {i + 1}"
                  onclick={() => recycleClip(c)}><Icon name="trash" /></button
                >
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <section class="card">
    <h2>Timeline</h2>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
    <svg
      viewBox="0 0 1000 112"
      role="group"
      aria-label="Moments and markers over the match"
      class:seekable={videoSrc}
      onclick={seekTimeline}
    >
      {#each gridlines as minute (minute)}
        <line class="grid" x1={x(minute)} x2={x(minute)} y1="4" y2="84" />
        <text x={x(minute)} y="104" text-anchor={minute === 0 ? 'start' : x(minute) > 960 ? 'end' : 'middle'}>
          {clock(minute)}
        </text>
      {/each}
      {#each match.moments as moment, i (i)}
        {@const start = momentStart(moment)}
        {#if playable(start)}
          <rect
            class="band {category(moment.label)} live"
            x={x(moment.startS)}
            y="10"
            width={Math.max(4, x(moment.endS) - x(moment.startS))}
            height="38"
            rx="6"
            role="button"
            tabindex="0"
            aria-label="Play {words(moment.label)} from {clock(moment.startS)}"
            {...seeker(start)}
          >
            <title>{words(moment.label)} {clock(moment.startS)}–{clock(moment.endS)}</title>
          </rect>
        {:else}
          <rect
            class="band {category(moment.label)}"
            x={x(moment.startS)}
            y="10"
            width={Math.max(4, x(moment.endS) - x(moment.startS))}
            height="38"
            rx="6"
          >
            <title>{words(moment.label)} {clock(moment.startS)}–{clock(moment.endS)}</title>
          </rect>
        {/if}
      {/each}
      {#each match.markers as marker (marker.id)}
        {@const start = markerStart(marker)}
        {#if playable(start)}
          <circle
            class="{category(marker.label)} live"
            cx={x(marker.gameTimeS)}
            cy="66"
            r="7"
            role="button"
            tabindex="0"
            aria-label="Play {words(marker.label)} at {clock(marker.gameTimeS)}"
            {...seeker(start)}
          >
            <title>{words(marker.label)} at {clock(marker.gameTimeS)}</title>
          </circle>
        {:else}
          <circle class={category(marker.label)} cx={x(marker.gameTimeS)} cy="66" r="7">
            <title>{words(marker.label)} at {clock(marker.gameTimeS)}</title>
          </circle>
        {/if}
      {/each}
      {#if range && offset != null}
        <rect
          class="marked"
          x={x(range.from + offset)}
          y="6"
          width={Math.max(2, x(range.to + offset) - x(range.from + offset))}
          height="46"
          rx="6"
        >
          <title>marked for a clip: {gameClock(range.from)}–{gameClock(range.to)}</title>
        </rect>
      {/if}
      {#if playhead != null}
        <line class="playhead" x1={x(playhead)} x2={x(playhead)} y1="2" y2="86" />
      {/if}
    </svg>
    <div class="legend">
      <span class="fight"><i></i>Kills and fights</span>
      <span class="objective"><i></i>Objectives</span>
      <span class="assist"><i></i>Assists</span>
      <span class="death"><i></i>Deaths</span>
      <span class="win"><i></i>Victory</span>
      {#if videoSrc}
        <span class="hint">Click a moment, a marker or anywhere on the timeline to watch it.</span>
      {:else if playerSrc}
        <span class="hint">Click a moment or a marker to watch its clip.</span>
      {/if}
    </div>
  </section>

  <section class="card">
    <h2>Moments <span class="count">{match.moments.length}</span></h2>
    {#if typeof saveError?.key === 'number'}<p class="error small">{saveError.text}</p>{/if}
    {#if match.moments.length}
      <table>
        <thead>
          <tr>
            {#if playerSrc}<th class="act"><span class="sr">Play</span></th>{/if}
            {#if videoSrc}<th class="act"><span class="sr">Clip</span></th>{/if}
            <th class="num">#</th><th>Highlight</th><th class="num">From</th><th class="num">To</th><th class="num"
              >Length</th
            ><th class="num">Markers</th>
          </tr>
        </thead>
        <tbody>
          {#each match.moments as moment, i (i)}
            {@const start = momentStart(moment)}
            {@const end = momentEnd(moment)}
            {@const saved = savedFor(start, end)}
            <tr>
              {#if playerSrc}
                <td class="act">
                  {#if playable(start)}
                    <button
                      type="button"
                      class="icon-btn round"
                      title="Play"
                      aria-label="Play moment {i + 1}"
                      onclick={() => seek(start)}><Icon name="play" size={18} /></button
                    >
                  {/if}
                </td>
              {/if}
              {#if videoSrc}
                <td class="act">
                  {#if saved}
                    <button
                      type="button"
                      class="icon-btn round done"
                      title="Saved as a clip: play it"
                      aria-label="Play the clip of moment {i + 1}"
                      onclick={() => show(saved)}><Icon name="check" size={18} /></button
                    >
                  {:else if start != null && end != null}
                    <button
                      type="button"
                      class="icon-btn round"
                      title="Save as a clip"
                      aria-label="Save moment {i + 1} as a clip"
                      disabled={saving != null}
                      onclick={() => saveMoment(moment, i)}
                      >{#if saving === i}…{:else}<Icon name="scissors" size={18} />{/if}</button
                    >
                  {/if}
                </td>
              {/if}
              <td class="num muted">{i + 1}</td>
              <td class={category(moment.label)}><i></i><span class="label">{words(moment.label)}</span></td>
              <td class="num">{clock(moment.startS)}</td>
              <td class="num">{clock(moment.endS)}</td>
              <td class="num">{(moment.endS - moment.startS).toFixed(1)} s</td>
              <td class="num">{moment.markerIds.length}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <p class="muted">No highlights in this match.</p>
    {/if}
  </section>

  {#if match.markers.length}
    <section class="card">
      <details>
        <summary>{match.markers.length} marker{match.markers.length === 1 ? '' : 's'}</summary>
        <table>
          <thead>
            <tr>
              {#if playerSrc}<th class="act"><span class="sr">Play</span></th>{/if}
              <th class="num">Time</th><th>Marker</th><th>Type</th><th class="num">Window</th><th class="num"
                >Riot events</th
              >
            </tr>
          </thead>
          <tbody>
            {#each match.markers as marker (marker.id)}
              {@const start = markerStart(marker)}
              <tr>
                {#if playerSrc}
                  <td class="act">
                    {#if playable(start)}
                      <button
                        type="button"
                        class="icon-btn round"
                        title="Play"
                        aria-label="Play {words(marker.label)} at {clock(marker.gameTimeS)}"
                        onclick={() => seek(start)}><Icon name="play" size={18} /></button
                      >
                    {/if}
                  </td>
                {/if}
                <td class="num">{clock(marker.gameTimeS)}</td>
                <td class={category(marker.label)}>
                  <i></i><span class="label">{words(marker.label)}</span>
                  {#if marker.stolen}<span class="muted">(stolen)</span>{/if}
                  {#if marker.byTeam}<span class="muted">(team)</span>{/if}
                </td>
                <td class="muted">{words(marker.type)}</td>
                <td class="num">−{marker.preS} s / +{marker.postS} s</td>
                <td class="num muted">{marker.riotEventIds.join(', ')}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </details>
    </section>
  {/if}
  {#if !videoSrc}
    <p class="muted novideo">
      {#if match.source === 'recording'}
        Fixture recordings have no video.
      {:else if match.source === 'outplayed' && !match.videoRemoved}
        This match's Outplayed video is no longer at {match.recording?.path}.
      {:else if match.videoRemoved}
        The cleanup sent this match's full recording to the Recycle Bin on {when(match.videoRemoved.atMs)}
        ({match.videoRemoved.reason === 'age' ? 'older than the days kept' : 'over the size limit'}).
        {#if clips.length}The clips saved from it are kept.{/if}
      {:else if clips.length}
        This match's recording is gone; the clips saved from it are kept.
      {:else if match.recording}
        This match was recorded, but its video file is no longer in the match folder.
      {:else}
        This match wasn't recorded (capture was off or unavailable).
      {/if}
    </p>
  {/if}
{/if}

<style>
  .topbar {
    margin: -6px 0 14px -10px;
  }
  .small {
    font-size: 14px;
  }
  .count {
    margin-left: 6px;
    color: var(--muted);
    font-weight: 500;
  }

  /* Header. */
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 20px 40px;
    border-left: 5px solid var(--line);
  }
  .head.won {
    border-left-color: var(--win);
  }
  .head.lost {
    border-left-color: var(--lose);
  }
  .who {
    flex: 1 1 260px;
    min-width: 0;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .title h1 {
    margin: 0;
    font-size: 34px;
    line-height: 1.15;
  }
  .sub {
    margin-top: 4px;
    color: var(--muted);
    font-size: 16px;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    height: 38px;
    padding: 0 18px;
    border-radius: 999px;
    font-size: 15px;
    font-weight: 800;
    letter-spacing: 0.8px;
    text-transform: uppercase;
    background: var(--raised);
    border: 1px solid var(--line);
  }
  .head.won .badge {
    background: var(--win);
    border-color: var(--win);
    color: var(--on-strong);
  }
  .head.lost .badge {
    background: var(--lose);
    border-color: var(--lose);
    color: var(--on-strong);
  }
  .outcome .small {
    margin-top: 6px;
  }
  .stats {
    display: flex;
    gap: 34px;
    flex-wrap: wrap;
  }
  .stats div {
    display: grid;
    gap: 2px;
  }
  .stats b {
    font-size: 26px;
    font-variant-numeric: tabular-nums;
    line-height: 1.15;
  }
  .stats span {
    color: var(--muted);
    font-size: 13px;
    letter-spacing: 0.5px;
    text-transform: uppercase;
  }

  /* Player. */
  .player video {
    display: block;
    width: 100%;
    aspect-ratio: 16 / 9;
    max-height: 72vh;
    background: #000;
    border-radius: 10px;
  }
  .playing {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    margin-bottom: 12px;
    font-size: 16px;
  }
  .trim {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 12px;
    margin: 16px 0 6px;
    font-size: 15px;
  }
  .trim .time {
    min-width: 4.2em;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .info {
    margin-top: 10px;
    overflow-wrap: anywhere;
  }

  /* Clips. */
  .clips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 18px;
  }
  .clips li {
    border: 1px solid var(--line);
    border-radius: 12px;
    overflow: hidden;
    background: var(--raised);
  }
  .clips li.current {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .thumb {
    position: relative;
    display: block;
    width: 100%;
    aspect-ratio: 16 / 9;
    padding: 0;
    border: 0;
    background: #000;
    color: #fff;
    cursor: pointer;
  }
  .thumb:disabled {
    cursor: default;
    opacity: 0.6;
  }
  .thumb img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .nothumb {
    font-size: 14px;
  }
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.25);
    opacity: 0;
    transition: opacity 120ms;
  }
  .thumb:hover .overlay,
  .thumb:focus-visible .overlay {
    opacity: 1;
  }
  .length {
    position: absolute;
    right: 8px;
    bottom: 8px;
    padding: 2px 8px;
    border-radius: 6px;
    background: rgba(0, 0, 0, 0.72);
    color: #fff;
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .clip-info {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 14px 4px;
    font-size: 16px;
  }
  .clip-info .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .clip-actions {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 4px 8px 10px;
  }
  .recycle {
    margin-left: auto;
  }
  .notice {
    color: var(--accent);
    margin: 0 0 12px;
  }

  /* Timeline. */
  svg {
    display: block;
    width: 100%;
    height: auto;
  }
  svg.seekable {
    cursor: pointer;
  }
  svg text {
    fill: var(--muted);
    font-size: 12px;
  }
  .grid {
    stroke: var(--line);
  }
  rect.band {
    opacity: 0.5;
  }
  .live:hover,
  .live:focus-visible {
    opacity: 0.9;
    outline: none;
  }
  circle.live:hover,
  circle.live:focus-visible {
    stroke: var(--text);
  }
  .playhead {
    stroke: var(--text);
    stroke-width: 2.5;
    pointer-events: none;
  }
  rect.marked {
    fill: var(--accent);
    opacity: 0.25;
    stroke: var(--accent);
    stroke-width: 1.5;
    pointer-events: none;
  }
  circle {
    stroke: var(--card);
    stroke-width: 2;
  }
  .fight {
    fill: var(--fight);
    color: var(--fight);
  }
  .objective {
    fill: var(--objective);
    color: var(--objective);
  }
  .assist {
    fill: var(--assist);
    color: var(--assist);
  }
  .death {
    fill: var(--death);
    color: var(--death);
  }
  .win {
    fill: var(--win);
    color: var(--win);
  }
  .custom {
    fill: var(--accent);
    color: var(--accent);
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 20px;
    margin-top: 10px;
    font-size: 14px;
  }
  .legend span {
    color: var(--muted);
  }
  .hint {
    flex-basis: 100%;
  }
  i {
    display: inline-block;
    width: 12px;
    height: 12px;
    border-radius: 4px;
    margin-right: 8px;
    background: currentColor;
    vertical-align: -1px;
  }
  .legend .fight i {
    background: var(--fight);
  }
  .legend .objective i {
    background: var(--objective);
  }
  .legend .assist i {
    background: var(--assist);
  }
  .legend .death i {
    background: var(--death);
  }
  .legend .win i {
    background: var(--win);
  }

  /* Tables. */
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 15px;
  }
  th {
    text-align: left;
    color: var(--muted);
    font-weight: 600;
    font-size: 13px;
    letter-spacing: 0.4px;
    text-transform: uppercase;
  }
  th,
  td {
    padding: 8px 10px;
    border-bottom: 1px solid var(--line);
  }
  tbody tr:hover {
    background: var(--hover);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .act {
    width: 52px;
    padding-right: 0;
  }
  .icon-btn.round {
    border-radius: 50%;
    border-color: var(--line);
    background: var(--raised);
    color: var(--accent);
  }
  .icon-btn.round.done {
    color: var(--win);
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
  .label {
    color: var(--text);
    text-transform: capitalize;
    font-weight: 600;
  }
  details summary {
    cursor: pointer;
    font-weight: 600;
    font-size: 17px;
  }
  details table {
    margin-top: 12px;
  }
  .novideo {
    font-size: 15px;
  }
</style>
