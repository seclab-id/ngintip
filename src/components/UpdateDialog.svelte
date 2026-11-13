<script>
  // Shown once at startup when GitHub reports a release newer than the running build.
  // Three ways out, deliberately: download now, be asked again next launch, or never
  // again for this version. There is no "disable update checks" here - the check itself
  // is one request and the user can still ignore the dialog forever via Skip.
  import Modal from './Modal.svelte';
  import { BRAND } from '../lib/brand.js';
  import { store } from '../lib/store.svelte.js';

  let u = $derived(store.update);
</script>

<Modal title="Update available" width={480} onclose={() => store.dismissUpdate()}>
  <div class="body">
    <p class="lead">
      A newer {BRAND.name} has been published.
    </p>

    <div class="vers">
      <div class="col">
        <span class="lbl">Installed</span>
        <span class="num mono">v{u.current}</span>
      </div>
      <span class="arrow" aria-hidden="true">&rarr;</span>
      <div class="col new">
        <span class="lbl">Latest</span>
        <span class="num mono">v{u.latest}</span>
      </div>
    </div>

    {#if u.name && u.name !== `v${u.latest}` && u.name !== u.latest}
      <p class="relname">{u.name}</p>
    {/if}

    <p class="note">
      The download page opens in your browser. Your projects and settings are untouched
      by an update.
    </p>

    <div class="acts">
      <button class="btn primary" onclick={() => store.downloadUpdate()}>Download</button>
      <button class="btn ghost" onclick={() => store.dismissUpdate()}>Remind me later</button>
      <button class="btn ghost sm skip" onclick={() => store.skipUpdate()}>Skip v{u.latest}</button>
    </div>
  </div>
</Modal>

<style>
  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .lead {
    margin: 0;
    font-size: 13px;
    color: var(--text, #e6e6e6);
  }
  .vers {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 18px;
    padding: 14px 10px;
    border: 1px solid var(--border, #2a2e38);
    border-radius: 8px;
    background: var(--bg, #12141a);
  }
  .col {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
  }
  .lbl {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-dim, #8b90a0);
  }
  .num {
    font-size: 15px;
    font-weight: 700;
    color: var(--text-dim, #8b90a0);
  }
  .col.new .num {
    color: var(--accent, #e8722c);
  }
  .arrow {
    font-size: 16px;
    color: var(--text-dim, #8b90a0);
  }
  .relname {
    margin: 0;
    text-align: center;
    font-size: 12px;
    font-weight: 600;
    color: var(--text, #e6e6e6);
  }
  .note {
    margin: 0;
    font-size: 11.5px;
    line-height: 1.5;
    color: var(--text-dim, #8b90a0);
  }
  .acts {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .acts .skip {
    margin-left: auto;
  }
</style>
