<script>
  import { onMount, untrack } from 'svelte';
  import { convertFileSrc, invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { clock, when } from './lib/format.js';

  let { onOpen } = $props();

  /** Every row is this tall, so the list can place any of them without measuring. */
  const ROW = 64;
  /** Matches per query. */
  const PAGE = 100;
  /** Rows drawn beyond the visible ones, each way. */
  const OVERSCAN = 6;

  const noFilters = () => ({
    champion: '',
    queue: '',
    result: '',
    hasClips: false,
    hasMultikill: false,
    from: '',
    to: '',
    sort: 'newest',
  });
  let filters = $state(noFilters());
  let facets = $state({ champions: [], queues: [] });
  /** How many matches pass the filters; null until the first page arrives. */
  let total = $state(null);
  /** Loaded pages by number. */
  let pages = $state({});
  let error = $state('');
  let scrollTop = $state(0);
  let height = $state(0);
  let scroller = $state(null);
  /** Pages being fetched, and the filters they were asked with (late answers are dropped). */
  let fetching = new Set();
  let generation = 0;
  /** Thumbnails already asked for. */
  const asked = new Set();

  const filtered = $derived(
    !!(
      filters.champion ||
      filters.queue ||
      filters.result ||
      filters.hasClips ||
      filters.hasMultikill ||
      filters.from ||
      filters.to
    ),
  );
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN));
  const last = $derived(Math.min(total ?? 0, Math.ceil((scrollTop + height) / ROW) + OVERSCAN));
  const rows = $derived(
    Array.from({ length: Math.max(0, last - first) }, (_, k) => {
      const index = first + k;
      return { index, match: pages[Math.floor(index / PAGE)]?.[index % PAGE] ?? null };
    }),
  );

  /** A date field (local day) as Unix ms; `next` gives the day after's start. */
  function dayMs(value, next = false) {
    if (!value) return null;
    const day = new Date(`${value}T00:00:00`);
    if (next) day.setDate(day.getDate() + 1);
    return day.getTime();
  }

  function query(page) {
    return {
      offset: page * PAGE,
      limit: PAGE,
      champion: filters.champion || null,
      queue: filters.queue || null,
      result: filters.result || null,
      hasClips: filters.hasClips,
      hasMultikill: filters.hasMultikill,
      fromMs: dayMs(filters.from),
      toMs: dayMs(filters.to, true),
      sort: filters.sort,
    };
  }

  async function load(page) {
    if (pages[page] || fetching.has(page)) return;
    fetching.add(page);
    const asOf = generation;
    try {
      const result = await invoke('list_matches', { query: query(page) });
      if (asOf !== generation) return;
      error = '';
      total = result.total;
      pages[page] = result.items;
      // For profiling: when the list first had rows (DevTools' performance timeline).
      if (!performance.getEntriesByName('downplayed:first-rows').length) {
        performance.mark('downplayed:first-rows');
      }
    } catch (e) {
      if (asOf === generation) error = String(e);
    } finally {
      fetching.delete(page);
    }
  }

  function loadFacets() {
    invoke('match_filters')
      .then((value) => (facets = value))
      .catch(() => {});
  }

  /** Drops what was loaded; the visible pages come again (placeholders meanwhile).
   *  `top`: back to the start. */
  function reload(top) {
    generation += 1;
    fetching = new Set();
    pages = {};
    if (top) {
      if (scroller) scroller.scrollTop = 0;
      scrollTop = 0;
    }
    load(0);
  }

  // Any change of a filter or the sort starts the list over. (Not from the inputs'
  // change handlers: those can run before the bound value has changed.)
  let filtersSeen = false;
  $effect(() => {
    JSON.stringify(filters);
    if (!filtersSeen) {
      filtersSeen = true;
      return;
    }
    untrack(() => reload(true));
  });

  // The pages the visible rows are on.
  $effect(() => {
    if (total === null) return;
    const from = Math.floor(first / PAGE);
    const to = Math.floor(Math.max(first, last - 1) / PAGE);
    for (let page = from; page <= to; page++) load(page);
  });

  // Thumbnails for the recorded matches on screen that have none yet.
  $effect(() => {
    const ids = rows
      .map((row) => row.match)
      .filter((match) => match?.video && !match.thumbnailPath && !asked.has(match.id))
      .map((match) => match.id);
    if (!ids.length) return;
    ids.forEach((id) => asked.add(id));
    invoke('request_thumbnails', { ids }).catch(() => {});
  });

  onMount(() => {
    loadFacets();
    load(0);
    const stops = [];
    listen('matches-changed', () => {
      loadFacets();
      reload(false);
    }).then((stop) => stops.push(stop));
    listen('match-thumbnail', (event) => {
      for (const items of Object.values(pages)) {
        const match = items.find((m) => m.id === event.payload.id);
        if (match) match.thumbnailPath = event.payload.thumbnailPath;
      }
    }).then((stop) => stops.push(stop));
    return () => stops.forEach((stop) => stop());
  });

  function clear() {
    filters = noFilters();
  }

  const initial = (name) => (name ? name.slice(0, 1).toUpperCase() : '?');
</script>

<section class="card">
  <div class="top">
    <h2>Matches</h2>
    {#if total !== null}<span class="muted">{total}{filtered ? ' shown' : ''}</span>{/if}
  </div>
  <div class="filters" role="group" aria-label="Filter and sort the matches">
    <select bind:value={filters.champion} aria-label="Champion">
      <option value="">All champions</option>
      {#each facets.champions as champion (champion)}<option value={champion}>{champion}</option>{/each}
    </select>
    <select bind:value={filters.queue} aria-label="Queue">
      <option value="">All queues</option>
      {#each facets.queues as queue (queue)}<option value={queue}>{queue}</option>{/each}
    </select>
    <select bind:value={filters.result} aria-label="Result">
      <option value="">Any result</option>
      <option value="win">Wins</option>
      <option value="lose">Losses</option>
      <option value="unknown">Unknown</option>
    </select>
    <label><input type="checkbox" bind:checked={filters.hasClips} /> clips</label>
    <label
      ><input type="checkbox" bind:checked={filters.hasMultikill} /> multikill</label
    >
    <label class="date">from <input type="date" bind:value={filters.from} /></label>
    <label class="date">to <input type="date" bind:value={filters.to} /></label>
    <select bind:value={filters.sort} aria-label="Sort">
      <option value="newest">Newest</option>
      <option value="oldest">Oldest</option>
      <option value="longest">Longest</option>
      <option value="most_kills">Most kills</option>
    </select>
    {#if filtered}<button type="button" class="link" onclick={clear}>clear</button>{/if}
  </div>

  {#if error}<p class="error">{error}</p>{/if}
  {#if total === null}
    <p class="muted">Loading…</p>
  {:else if total === 0}
    <p class="muted">
      {#if filtered}
        No matches fit these filters.
      {:else}
        No matches yet. Play a game (Practice Tool works) and its review opens here afterwards.
      {/if}
    </p>
  {:else}
    <div
      class="scroller"
      bind:this={scroller}
      bind:clientHeight={height}
      onscroll={(event) => (scrollTop = event.currentTarget.scrollTop)}
    >
      <ul class="matches" style:height="{total * ROW}px">
        {#each rows as row (row.index)}
          {@const match = row.match}
          <li style:transform="translateY({row.index * ROW}px)">
            {#if match}
              <button type="button" onclick={() => onOpen(match.id)}>
                <span class="thumb">
                  {#if match.thumbnailPath}
                    <img src={convertFileSrc(match.thumbnailPath)} alt="" loading="lazy" decoding="async" />
                  {:else}
                    <span class="initial">{initial(match.champion)}</span>
                  {/if}
                </span>
                <span class="result" class:win={match.result === 'win'} class:lose={match.result === 'lose'}>
                  {match.result === 'win' ? 'Win' : match.result === 'lose' ? 'Loss' : '–'}
                </span>
                <span class="who">
                  <b>{match.champion ?? 'Unknown champion'}</b>
                  <span class="muted">{match.queue ?? match.gameMode ?? ''}</span>
                </span>
                <span class="num">
                  {match.kills ?? '–'} / {match.deaths ?? '–'} / {match.assists ?? '–'}
                </span>
                <span class="num muted">{clock(match.durationS)}</span>
                <span class="tags">
                  {#if match.multikills}<span class="tag hot">{match.multikills} multikill{match.multikills === 1 ? '' : 's'}</span>{/if}
                  {#if match.clips}<span class="tag">{match.clips} clip{match.clips === 1 ? '' : 's'}{match.favoriteClips ? ' ★' : ''}</span>{/if}
                  {#if match.source === 'recording'}<span class="tag">recording</span>{/if}
                </span>
                <span class="when muted">{when(match.whenMs)}</span>
              </button>
            {:else}
              <div class="placeholder"></div>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</section>

<style>
  .top {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .top h2 {
    margin: 0 0 8px;
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
    margin-bottom: 10px;
    font-size: 13px;
  }
  .filters select,
  .filters input[type='date'] {
    font: inherit;
    color: var(--text);
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 3px 6px;
  }
  .filters label {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--muted);
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
  .scroller {
    position: relative;
    height: min(62vh, 640px);
    overflow-y: auto;
    contain: strict;
  }
  .matches {
    position: relative;
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 64px;
    box-sizing: border-box;
    border-bottom: 1px solid var(--line);
  }
  button {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    height: 100%;
    display: grid;
    /* Fixed columns, so rows line up (each row is its own grid). */
    grid-template-columns: 96px 40px minmax(0, 1fr) 96px 56px 170px 150px;
    align-items: center;
    gap: 12px;
    padding: 4px 6px;
    border-radius: 6px;
    cursor: pointer;
  }
  button:hover,
  button:focus-visible {
    background: var(--hover);
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
  }
  .placeholder {
    height: 100%;
    margin: 0 6px;
    background: linear-gradient(90deg, var(--hover), transparent);
    border-radius: 6px;
    opacity: 0.6;
  }
  .thumb {
    width: 96px;
    height: 54px;
    border-radius: 5px;
    overflow: hidden;
    background: var(--hover);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .initial {
    font-weight: 700;
    color: var(--muted);
  }
  .result {
    font-weight: 700;
    text-transform: uppercase;
    font-size: 12px;
    color: var(--muted);
  }
  .result.win {
    color: var(--win);
  }
  .result.lose {
    color: var(--lose);
  }
  .who {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .who b,
  .who span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .tags {
    display: flex;
    gap: 4px;
    justify-content: flex-end;
    overflow: hidden;
  }
  .tag {
    display: inline-block;
    padding: 0 6px;
    border: 1px solid var(--line);
    border-radius: 999px;
    font-size: 11px;
    white-space: nowrap;
  }
  .tag.hot {
    border-color: var(--fight);
    color: var(--fight);
  }
  .when {
    text-align: right;
    font-size: 12px;
  }
  @media (max-width: 860px) {
    button {
      grid-template-columns: 96px 40px minmax(0, 1fr) auto;
    }
    button > :nth-child(5),
    button > :nth-child(6),
    button > :nth-child(7) {
      display: none;
    }
  }
</style>
