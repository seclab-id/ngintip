import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';

const app = mount(App, { target: document.getElementById('app') });

// The UI is now in the DOM (the project chooser renders synchronously), so fade
// out and remove the startup splash from index.html.
const splash = document.getElementById('splash');
if (splash) {
  splash.classList.add('hide');
  splash.addEventListener('transitionend', () => splash.remove(), { once: true });
  setTimeout(() => splash.remove(), 400); // fallback if the transition doesn't fire
}

export default app;
