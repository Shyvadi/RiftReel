<script>
  import { clock, when } from './lib/format.js';

  let { matches, loading, onOpen } = $props();
</script>

<section class="card">
  <h2>Matches</h2>
  {#if loading && !matches.length}
    <p class="muted">Loading…</p>
  {:else if !matches.length}
    <p class="muted">No matches yet. Play a game (Practice Tool works) and its review opens here afterwards.</p>
  {:else}
    <ul class="matches">
      {#each matches as match (match.id)}
        <li>
          <button type="button" onclick={() => onOpen(match.id)}>
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
            <span class="num muted">{match.moments} moment{match.moments === 1 ? '' : 's'}</span>
            <span class="when muted">
              {when(match.whenMs)}
              {#if match.source === 'recording'}<span class="tag">recording</span>{/if}
            </span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .matches {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li + li {
    border-top: 1px solid var(--line);
  }
  button {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    display: grid;
    /* Fixed columns, so rows line up (each row is its own grid). */
    grid-template-columns: 52px minmax(0, 1fr) 96px 56px 92px 200px;
    align-items: center;
    gap: 12px;
    padding: 9px 6px;
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
  .when {
    text-align: right;
    font-size: 12px;
  }
  .tag {
    display: inline-block;
    margin-left: 6px;
    padding: 0 6px;
    border: 1px solid var(--line);
    border-radius: 999px;
    font-size: 11px;
  }
  @media (max-width: 720px) {
    button {
      grid-template-columns: 44px 1fr auto;
    }
    button > :nth-child(4),
    button > :nth-child(5),
    button > :nth-child(6) {
      display: none;
    }
  }
</style>
