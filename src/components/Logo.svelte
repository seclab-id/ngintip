<script>
  // ============================================================================
  //  BRAND LOGO - the single place to change the app's logo.
  // ============================================================================
  //  Used by the Sidebar and the start screen (ProjectChooser). Change it once
  //  here and it updates everywhere in the running app.
  //
  //  To change the logo:
  //   - Swap the <svg> below for your own icon markup (keep viewBox="0 0 24 24"
  //     for the sizing to stay correct), OR
  //   - Replace it with an image file:  <img src="/my-logo.svg" alt="" />
  //     (drop the file in the project root / public and reference it by path).
  //
  //  The badge gradient uses the --logo-from / --logo-to tokens from src/app.css
  //  (edit those to recolour the badge for every theme at once).
  //
  //  NOTE: the pre-app loading splash in index.html keeps its OWN copy of this
  //  mark + gradient (it paints before any JS/CSS loads, so it can't import this
  //  component). If you change the logo, update index.html to match.
  // ============================================================================

  let {
    size = 36, // badge size in px
    radius = null, // corner radius (defaults to ~28% of size)
    shadow = false, // soft drop shadow under the badge
    title = '', // optional tooltip / accessible label
  } = $props();

  const r = $derived(radius ?? Math.round(size * 0.28));
  const mark = $derived(Math.round(size * 0.56)); // the icon is ~56% of the badge
</script>

<span
  class="brand-logo"
  style="width:{size}px; height:{size}px; border-radius:{r}px; box-shadow:{shadow
    ? '0 4px 14px rgb(99 88 255 / 0.35)'
    : 'none'}"
  {title}
  role={title ? 'img' : undefined}
  aria-label={title || undefined}
>
  <!-- The brand mark. Replace this <svg> (or use an <img>) to rebrand. -->
  <svg viewBox="0 0 24 24" style="width:{mark}px; height:{mark}px" aria-hidden="true">
    <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" />
    <circle cx="12" cy="12" r="3" />
  </svg>
</span>

<style>
  .brand-logo {
    display: grid;
    place-items: center;
    flex-shrink: 0;
    /* Brand badge gradient - recolour via the --logo-from / --logo-to tokens. */
    background: linear-gradient(135deg, var(--logo-from, #8b7bff), var(--logo-to, #4f46e5));
  }
  .brand-logo svg {
    display: block;
    fill: none;
    stroke: #fff;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
