// The only script on the page: a theme switch that remembers its choice.
//
// The document starts with no data-theme, which means "follow the system". The first
// click writes an explicit choice; from then on the CSS rules keyed on
// :root[data-theme=...] win over the prefers-color-scheme block. Storage can throw in a
// private window, so every access is guarded and the page works without it.
(function () {
  var KEY = 'ngintip.site.theme';
  var root = document.documentElement;
  var button = document.getElementById('theme');
  if (!button) return;

  function current() {
    if (root.dataset.theme === 'light' || root.dataset.theme === 'dark') return root.dataset.theme;
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }

  button.addEventListener('click', function () {
    var next = current() === 'dark' ? 'light' : 'dark';
    root.dataset.theme = next;
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', next === 'dark' ? '#0d0f14' : '#f5f6f9');
    try {
      localStorage.setItem(KEY, next);
    } catch (e) {
      /* private window: the choice lasts for this page view only */
    }
  });
})();

// Code blocks: colour them, and let people take them.
//
// Written here rather than pulled from a highlighter library because the site ships
// four short languages and a library is twenty times the weight of this file. The
// tokenizer walks the text once, trying a small list of patterns at each position;
// anything it does not recognise is copied through escaped, so a block can never
// inject markup. Without this script the blocks still read fine, they are just plainer.
(function () {
  var RULES = {
    py: [
      [/^#[^\n]*/, 'c'],
      [/^"""[\s\S]*?"""/, 's'],
      [/^"(?:[^"\\\n]|\\.)*"/, 's'],
      [/^'(?:[^'\\\n]|\\.)*'/, 's'],
      [/^\b(?:def|class|import|from|return|if|elif|else|for|while|in|not|and|or|None|True|False|try|except|finally|with|as|pass|lambda|raise|print)\b/, 'k'],
      [/^\b\d+\b/, 'n'],
    ],
    js: [
      [/^\/\/[^\n]*/, 'c'],
      [/^'(?:[^'\\\n]|\\.)*'/, 's'],
      [/^"(?:[^"\\\n]|\\.)*"/, 's'],
      [/^`(?:[^`\\]|\\.)*`/, 's'],
      [/^\b(?:const|let|var|function|return|if|else|for|while|new|class|export|import|from|true|false|null|undefined|typeof)\b/, 'k'],
      [/^\b\d+\b/, 'n'],
    ],
    txt: [
      [/^#[^\n]*/, 'c'],
      [/^"(?:[^"\\\n]|\\.)*"/, 's'],
    ],
  };

  function escape(s) {
    return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }

  function language(src) {
    if (/\bdef\s+\w+\s*\(|^\s*(?:import|from)\s+\w/m.test(src)) return 'py';
    if (/\b(?:const|function|=>)\b|^\s*\{\s*id:/m.test(src)) return 'js';
    return 'txt';
  }

  function highlight(src) {
    var rules = RULES[language(src)];
    var out = '';
    var i = 0;
    while (i < src.length) {
      var rest = src.slice(i);
      var hit = null;
      for (var r = 0; r < rules.length && !hit; r++) {
        var m = rules[r][0].exec(rest);
        if (m) hit = [m[0], rules[r][1]];
      }
      if (hit) {
        out += '<span class="' + hit[1] + '">' + escape(hit[0]) + '</span>';
        i += hit[0].length;
      } else {
        out += escape(src[i]);
        i += 1;
      }
    }
    return out;
  }

  var blocks = document.querySelectorAll('pre.code > code');
  for (var b = 0; b < blocks.length; b++) {
    (function (code) {
      var src = code.textContent;
      code.innerHTML = highlight(src);

      var button = document.createElement('button');
      button.type = 'button';
      button.className = 'copy';
      button.textContent = 'Copy';
      button.setAttribute('aria-label', 'Copy this snippet');
      button.addEventListener('click', function () {
        var done = function () {
          button.textContent = 'Copied';
          button.classList.add('done');
          setTimeout(function () {
            button.textContent = 'Copy';
            button.classList.remove('done');
          }, 1400);
        };
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(src).then(done, function () {
            button.textContent = 'Press Cmd+C';
          });
        } else {
          // No clipboard API (an old browser, or a page served over plain http):
          // select the text so the keyboard shortcut still works.
          var range = document.createRange();
          range.selectNodeContents(code);
          var sel = window.getSelection();
          sel.removeAllRanges();
          sel.addRange(range);
          button.textContent = 'Press Cmd+C';
        }
      });
      code.parentNode.appendChild(button);
    })(blocks[b]);
  }
})();
