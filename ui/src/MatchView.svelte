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

  $effect(() => {
    const wanted = id;
    match = null;
    error = '';
    videoError = '';
    videoTime = 0;
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

  // The asset protocol streams the file in ranged reads; it is never loaded whole.
  const videoSrc = $derived(match?.videoPath ? convertFileSrc(match.videoPath) : null);
  const markersById = $derived(new Map((match?.markers ?? []).map((m) => [m.id, m])));
  /** Game time at video time 0 (negative when recording began in the loading screen). */
  const offset = $derived.by(() => {
    const start = match?.recording?.gameTimeAtStartS;
    if (start != null) return start;
    const aligned = (match?.markers ?? []).find((m) => m.videoTimeS != null);
    return aligned ? aligned.gameTimeS - aligned.videoTimeS : null;
  });
  const playhead = $derived(videoSrc && offset != null ? videoTime + offset : null);

  /** Where a marker's clip starts in the video: its pre-roll before the event. */
  const markerStart = (marker) => (marker.videoTimeS == null ? null : marker.videoTimeS - marker.preS);
  /** A moment starts with its earliest marker's pre-roll. */
  function momentStart(moment) {
    const starts = moment.markerIds
      .map((markerId) => markersById.get(markerId))
      .filter(Boolean)
      .map(markerStart)
      .filter((start) => start != null);
    return starts.length ? Math.min(...starts) : null;
  }

  function seek(seconds) {
    if (!video || seconds == null) return;
    video.currentTime = Math.max(0, seconds);
    video.play().catch(() => {});
    video.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }

  /** A click on the timeline's background: play from that game time. */
  function seekTimeline(event) {
    if (!video || offset == null) return;
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

  {#if videoSrc}
    {@const recording = match.recording}
    <section class="card player">
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        src={videoSrc}
        controls
        preload="metadata"
        ontimeupdate={() => (videoTime = video.currentTime)}
        onerror={() => (videoError = 'This video could not be played.')}
      ></video>
      {#if videoError}<p class="error">{videoError}</p>{/if}
      <div class="muted small">
        {recording.width}×{recording.height} · {recording.fps} fps · {recording.encoder}
        {#if recording.droppedFrames}· {recording.droppedFrames} dropped frames{/if}
        {#if recording.error}· the recording ended early ({recording.error}){/if}
      </div>
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
        {#if videoSrc && start != null}
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
        {#if videoSrc && start != null}
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
      {#if videoSrc}<span>click a moment, a marker or anywhere on the timeline to watch it</span>{/if}
    </div>
  </section>

  <section class="card">
    <h3>Moments</h3>
    {#if match.moments.length}
      <table>
        <thead>
          <tr>
            {#if videoSrc}<th class="act"><span class="sr">play</span></th>{/if}
            <th class="num">#</th><th>highlight</th><th class="num">from</th><th class="num">to</th><th class="num">length</th><th class="num">markers</th>
          </tr>
        </thead>
        <tbody>
          {#each match.moments as moment, i (i)}
            {@const start = momentStart(moment)}
            <tr>
              {#if videoSrc}
                <td class="act">
                  {#if start != null}
                    <button type="button" class="play" aria-label="Play moment {i + 1}" onclick={() => seek(start)}>▶</button>
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
              {#if videoSrc}<th class="act"><span class="sr">play</span></th>{/if}
              <th class="num">time</th><th>marker</th><th>type</th><th class="num">window</th><th class="num">Riot events</th>
            </tr>
          </thead>
          <tbody>
            {#each match.markers as marker (marker.id)}
              {@const start = markerStart(marker)}
              <tr>
                {#if videoSrc}
                  <td class="act">
                    {#if start != null}
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
