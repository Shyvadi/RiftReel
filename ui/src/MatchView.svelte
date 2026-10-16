<script>
  import { convertFileSrc, invoke } from '@tauri-apps/api/core';
  import { category, clock, kda, when, words } from './lib/format.js';

  let { id, onBack } = $props();

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

<nav>
  <button type="button" class="back" onclick={onBack}>← All matches</button>
</nav>

{#if error}
  <p class="error">{error}</p>
{:else if !match}
  <p class="muted">Loading…</p>
{:else}
  <header class="card head">
    <div class="who">
      <h2>{match.champion ?? 'Unknown champion'}</h2>
      <div class="muted">
        {match.queue ?? match.gameMode ?? ''}
        {#if match.source === 'recording'}· recording{/if}
        {#if match.startedWallMs && match.source === 'game'}· {when(match.startedWallMs)}{/if}
      </div>
      {#if match.description}<div class="muted small">{match.description}</div>{/if}
    </div>
    <div class="outcome">
      <span class="badge" class:win={match.result === 'win'} class:lose={match.result === 'lose'}>
        {match.result === 'win' ? 'Victory' : match.result === 'lose' ? 'Defeat' : 'Result unknown'}
      </span>
      {#if match.resultSource}<div class="muted small">{sourceText[match.resultSource] ?? ''}</div>{/if}
    </div>
    <div class="stats">
      <div><b>{kda(match.kda)}</b><span>K / D / A</span></div>
      <div><b>{clock(match.durationS)}</b><span>length</span></div>
      <div><b>{match.team ?? '–'}</b><span>team</span></div>
      <div><b>{match.moments.length}</b><span>moments</span></div>
    </div>
  </header>

  {#if playerSrc}
    {@const recording = match.recording}
    <section class="card player">
      {#if shown}
        <div class="playing">
          <span>
            Clip {shownIndex + 1} · <span class="label">{words(shown.label)}</span>
            <span class="muted">· {gameClock(shown.fromS)}–{gameClock(shown.toS)}</span>
          </span>
          {#if videoSrc}
            <button type="button" class="back" onclick={() => show(null, shown.fromS + videoTime)}>Whole match</button>
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
          <button type="button" class="back" onclick={() => mark('in')}>Mark in</button>
          <span class="num">{markIn == null ? '–' : gameClock(markIn)}</span>
          <button type="button" class="back" onclick={() => mark('out')}>Mark out</button>
          <span class="num">{markOut == null ? '–' : gameClock(markOut)}</span>
          {#if range}<span class="muted num">{(range.to - range.from).toFixed(1)} s</span>{/if}
          <button
            type="button"
            class="save"
            disabled={!range || saving != null}
            onclick={() => saveClip('range', range.from, range.to, 'clip', null)}
          >
            {saving === 'range' ? 'Saving…' : 'Save clip'}
          </button>
          {#if markIn != null || markOut != null}
            <button type="button" class="link" onclick={() => ((markIn = null), (markOut = null))}>clear</button>
          {/if}
        </div>
        {#if saveError?.key === 'range'}<p class="error small">{saveError.text}</p>{/if}
      {/if}
      {#if recording}
        <div class="muted small">
          {recording.width}×{recording.height} · {recording.fps} fps · {recording.encoder}
          {#if recording.droppedFrames}· {recording.droppedFrames} dropped frames{/if}
          {#if recording.error}· the recording ended early ({recording.error}){/if}
        </div>
      {/if}
    </section>
  {/if}

  {#if clips.length}
    <section class="card">
      <h3>Clips <span class="muted">{clips.length}</span></h3>
      {#if clipError}<p class="error small">{clipError}</p>{/if}
      {#if clipNotice}<p class="notice small">{clipNotice}</p>{/if}
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
                <span class="nothumb">{c.path ? '▶' : 'missing'}</span>
              {/if}
              <span class="length">{Math.round(c.toS - c.fromS)} s</span>
            </button>
            <div class="clip-info">
              <span class="name {category(c.label)}"><i></i><span class="label">{words(c.label)}</span></span>
              <span class="muted num">{gameClock(c.fromS)}</span>
            </div>
            <div class="clip-actions">
              <button
                type="button"
                class="icon"
                class:on={c.favorite}
                aria-pressed={c.favorite}
                title={c.favorite ? 'Favorite' : 'Mark as favorite'}
                onclick={() => toggleFavorite(c)}>{c.favorite ? '★' : '☆'}</button
              >
              <button
                type="button"
                class="icon"
                disabled={!c.path}
                title="Show in Explorer"
                aria-label="Show clip {i + 1} in Explorer"
                onclick={() => reveal(c)}>📂</button
              >
              <button
                type="button"
                class="icon"
                disabled={!c.path}
                title="Copy (paste it anywhere)"
                aria-label="Copy clip {i + 1}"
                onclick={() => copyClip(c)}>📋</button
              >
              <button
                type="button"
                class="icon"
                disabled={!c.path}
                title="Save a copy as…"
                aria-label="Save a copy of clip {i + 1}"
                onclick={() => exportClip(c)}>💾</button
              >
              <button
                type="button"
                class="icon recycle"
                class:confirm={confirming === c.file}
                title="Move to the Recycle Bin"
                aria-label={confirming === c.file ? `Confirm: recycle clip ${i + 1}` : `Recycle clip ${i + 1}`}
                onclick={() => recycleClip(c)}>{confirming === c.file ? 'Recycle?' : '🗑'}</button
              >
            </div>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <section class="card">
    <h3>Timeline</h3>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
    <svg
      viewBox="0 0 1000 86"
      role="group"
      aria-label="Moments and markers over the match"
      class:seekable={videoSrc}
      onclick={seekTimeline}
    >
      {#each gridlines as minute (minute)}
        <line class="grid" x1={x(minute)} x2={x(minute)} y1="4" y2="64" />
        <text x={x(minute)} y="80" text-anchor={minute === 0 ? 'start' : x(minute) > 960 ? 'end' : 'middle'}>
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
            width={Math.max(3, x(moment.endS) - x(moment.startS))}
            height="30"
            rx="4"
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
            width={Math.max(3, x(moment.endS) - x(moment.startS))}
            height="30"
            rx="4"
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
            cy="52"
            r="5"
            role="button"
            tabindex="0"
            aria-label="Play {words(marker.label)} at {clock(marker.gameTimeS)}"
            {...seeker(start)}
          >
            <title>{words(marker.label)} at {clock(marker.gameTimeS)}</title>
          </circle>
        {:else}
          <circle class={category(marker.label)} cx={x(marker.gameTimeS)} cy="52" r="5">
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
          height="38"
          rx="4"
        >
          <title>marked for a clip: {gameClock(range.from)}–{gameClock(range.to)}</title>
        </rect>
      {/if}
      {#if playhead != null}
        <line class="playhead" x1={x(playhead)} x2={x(playhead)} y1="2" y2="66" />
      {/if}
    </svg>
    <div class="legend">
      <span class="fight"><i></i>kills and fights</span>
      <span class="objective"><i></i>objectives</span>
      <span class="assist"><i></i>assists</span>
      <span class="death"><i></i>deaths</span>
      <span class="win"><i></i>victory</span>
      {#if videoSrc}
        <span>click a moment, a marker or anywhere on the timeline to watch it; ✂ saves a moment as a clip</span>
      {:else if playerSrc}
        <span>click a moment or a marker to watch its clip</span>
      {/if}
    </div>
  </section>

  <section class="card">
    <h3>Moments</h3>
    {#if typeof saveError?.key === 'number'}<p class="error small">{saveError.text}</p>{/if}
    {#if match.moments.length}
      <table>
        <thead>
          <tr>
            {#if playerSrc}<th class="act"><span class="sr">play</span></th>{/if}
            {#if videoSrc}<th class="act"><span class="sr">clip</span></th>{/if}
            <th class="num">#</th><th>highlight</th><th class="num">from</th><th class="num">to</th><th class="num">length</th><th class="num">markers</th>
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
                    <button type="button" class="play" aria-label="Play moment {i + 1}" onclick={() => seek(start)}>▶</button>
                  {/if}
                </td>
              {/if}
              {#if videoSrc}
                <td class="act">
                  {#if saved}
                    <button
                      type="button"
                      class="play"
                      title="Saved as a clip: play it"
                      aria-label="Play the clip of moment {i + 1}"
                      onclick={() => show(saved)}>✓</button
                    >
                  {:else if start != null && end != null}
                    <button
                      type="button"
                      class="play"
                      title="Save as a clip"
                      aria-label="Save moment {i + 1} as a clip"
                      disabled={saving != null}
                      onclick={() => saveMoment(moment, i)}>{saving === i ? '…' : '✂'}</button
                    >
                  {/if}
                </td>
              {/if}
              <td class="num">{i + 1}</td>
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
              {#if playerSrc}<th class="act"><span class="sr">play</span></th>{/if}
              <th class="num">time</th><th>marker</th><th>type</th><th class="num">window</th><th class="num">Riot events</th>
            </tr>
          </thead>
          <tbody>
            {#each match.markers as marker (marker.id)}
              {@const start = markerStart(marker)}
              <tr>
                {#if playerSrc}
                  <td class="act">
                    {#if playable(start)}
                      <button type="button" class="play" aria-label="Play {words(marker.label)} at {clock(marker.gameTimeS)}" onclick={() => seek(start)}>▶</button>
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
    <p class="muted small">
      {#if match.source === 'recording'}
        Fixture recordings have no video.
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
  nav {
    margin: 4px 0 12px;
  }
  .back {
    border: 1px solid var(--line);
    background: var(--card);
    color: var(--text);
    border-radius: 999px;
    padding: 4px 12px;
    font: inherit;
    cursor: pointer;
  }
  .back:hover {
    background: var(--hover);
  }
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 16px 32px;
  }
  .who h2 {
    margin: 0;
    font-size: 22px;
  }
  .small {
    font-size: 12px;
    margin-top: 2px;
  }
  .badge {
    display: inline-block;
    padding: 4px 12px;
    border-radius: 999px;
    font-weight: 700;
    background: var(--line);
  }
  .badge.win {
    background: var(--win);
    color: var(--on-strong);
  }
  .badge.lose {
    background: var(--lose);
    color: var(--on-strong);
  }
  .stats {
    display: flex;
    gap: 24px;
    flex-wrap: wrap;
  }
  .stats b {
    display: block;
    font-size: 18px;
    font-variant-numeric: tabular-nums;
  }
  .stats span {
    color: var(--muted);
    font-size: 12px;
  }
  .player video {
    display: block;
    width: 100%;
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 6px;
    margin-bottom: 6px;
  }
  .trim {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    margin: 4px 0 8px;
    font-size: 13px;
  }
  .trim .num {
    min-width: 3.2em;
  }
  .save {
    border: 0;
    border-radius: 999px;
    padding: 5px 14px;
    background: var(--accent);
    color: var(--on-strong);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .save:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--muted);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }
  .play:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .playing {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 8px;
  }
  .clips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 12px;
  }
  .clips li {
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
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
    font-size: 13px;
  }
  .length {
    position: absolute;
    right: 6px;
    bottom: 6px;
    padding: 1px 6px;
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.7);
    color: #fff;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .clip-info {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px 0;
    font-size: 12px;
  }
  .clip-actions {
    display: flex;
    gap: 2px;
    padding: 2px 4px 4px;
  }
  .recycle {
    margin-left: auto;
  }
  .recycle.confirm {
    color: var(--lose);
    font-size: 12px;
    font-weight: 600;
  }
  .notice {
    color: var(--accent);
    margin: 0 0 8px;
  }
  .clip-info .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .icon {
    border: 0;
    background: none;
    color: var(--muted);
    border-radius: 4px;
    padding: 2px 4px;
    font: inherit;
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
  }
  .icon:hover,
  .icon:focus-visible {
    background: var(--hover);
    color: var(--text);
  }
  .icon.on {
    color: var(--warn);
  }
  .icon:disabled {
    opacity: 0.4;
    cursor: default;
  }
  h3 {
    margin: 0 0 10px;
    font-size: 14px;
  }
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
    font-size: 11px;
  }
  .grid {
    stroke: var(--line);
  }
  rect.band {
    opacity: 0.45;
  }
  .live:hover,
  .live:focus-visible {
    opacity: 0.85;
    outline: none;
  }
  circle.live:hover,
  circle.live:focus-visible {
    stroke: var(--text);
  }
  .playhead {
    stroke: var(--text);
    stroke-width: 2;
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
    stroke-width: 1.5;
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
    gap: 14px;
    margin-top: 6px;
    font-size: 12px;
    color: var(--muted);
  }
  .legend span {
    color: var(--muted);
  }
  i {
    display: inline-block;
    width: 10px;
    height: 10px;
    border-radius: 3px;
    margin-right: 6px;
    background: currentColor;
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
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    text-align: left;
    color: var(--muted);
    font-weight: 600;
    font-size: 12px;
  }
  th,
  td {
    padding: 6px 8px;
    border-bottom: 1px solid var(--line);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .act {
    width: 28px;
    padding-right: 0;
  }
  .play {
    border: 1px solid var(--line);
    background: var(--card);
    color: var(--accent);
    border-radius: 999px;
    width: 26px;
    height: 26px;
    padding: 0;
    font-size: 11px;
    cursor: pointer;
  }
  .play:hover,
  .play:focus-visible {
    background: var(--hover);
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
  }
  .error {
    color: var(--lose);
  }
</style>
