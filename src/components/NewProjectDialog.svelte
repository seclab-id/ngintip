<script>
  // "New project": name the file (typed, or a random dated name) and pick its folder.
  // Create hands the full path back to store.newProject(), which starts a blank project
  // saved there right away. "project pentest x" is saved as project_pentest_x.ngintip.
  import { onMount, tick } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { store } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import { joinPath, projectFileName, randomProjectName } from '../lib/projectName.js';
  import Modal from './Modal.svelte';

  let { dialog } = $props();
  let name = $state('');
  // svelte-ignore state_referenced_locally
  let dir = $state(dialog.dir || '');
  let input = $state();
  let busy = $state(false);
  let error = $state('');

  let file = $derived(projectFileName(name));

  onMount(async () => {
    input?.focus();
    if (!dir) dir = await api.defaultProjectDir().catch(() => '');
  });

  function random() {
    name = randomProjectName();
    error = '';
    input?.focus();
  }

  async function chooseDir() {
    const picked = await open({ title: 'Folder for the new project', directory: true, multiple: false, defaultPath: dir || undefined });
    if (typeof picked === 'string' && picked) dir = picked;
  }

  async function create() {
    if (!file || !dir || busy) return;
    busy = true;
    error = '';
    try {
      const path = joinPath(dir, file);
      if (await api.pathExists(path)) {
        const ok = await store.confirm({
          title: `Replace "${file}"?`,
          message: `A project with this name is already in ${dir}. Replacing it overwrites that file and everything saved in it. This cannot be undone.`,
          yes: 'Yes, replace',
        });
        if (!ok) {
          // Keep the dialog so another name can be typed.
          await tick();
          input?.select();
          return;
        }
      }
      store.answerNewProject(path);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal title="New project" width={480} onclose={() => store.answerNewProject(null)}>
  <form class="np" onsubmit={(e) => (e.preventDefault(), create())}>
    <label class="lbl" for="np-name">Name</label>
    <div class="row">
      <input
        id="np-name"
        class="input grow"
        bind:this={input}
        bind:value={name}
        oninput={() => (error = '')}
        placeholder="e.g. project pentest x"
        spellcheck="false"
        autocomplete="off"
      />
      <button type="button" class="btn" title="Random dated name: day-month-year-xxxxxx" onclick={random}>Random</button>
    </div>

    <span class="lbl">File</span>
    <div class="file mono" class:empty={!file}>{file || 'Type a name or press Random'}</div>

    <span class="lbl">Folder</span>
    <div class="row">
      <span class="dir mono grow" title={dir}>{dir || '…'}</span>
      <button type="button" class="btn ghost sm" onclick={chooseDir}>Change…</button>
    </div>

    {#if error}<p class="err">{error}</p>{/if}

    <div class="actions">
      <button type="button" class="btn ghost" onclick={() => store.answerNewProject(null)}>Cancel</button>
      <button type="submit" class="btn primary" disabled={!file || !dir || busy}>Create project</button>
    </div>
  </form>
</Modal>

<style>
  .np {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: 10px 12px;
  }
  .lbl {
    color: var(--text-dim);
    font-size: 12px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .file {
    overflow-wrap: anywhere;
    color: var(--text);
  }
  .file.empty {
    color: var(--text-faint);
    font-family: var(--sans);
  }
  .dir {
    overflow: hidden;
    color: var(--text-dim);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .err {
    grid-column: 1 / -1;
    margin: 0;
    color: var(--red);
    font-size: 12px;
  }
  .actions {
    grid-column: 1 / -1;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
</style>
