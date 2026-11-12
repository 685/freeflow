export const escapeHtml = value => String(value ?? '').replace(/[&<>"']/g, char =>
  ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]);

const words = new Set((
  'abstract as assert async await break case catch class const continue covariant default deferred do dynamic else enum ' +
  'export extends extension external factory false final finally for Function get hide if implements import in interface ' +
  'is late library mixin new null on operator part required rethrow return sealed set show static super switch sync this ' +
  'throw true try typedef var void while with yield'
).split(' '));

function highlightDart(source) {
  if (source.length > 180000) return escapeHtml(source);
  const tokens = /\/\/[^\r\n]*|\/\*[\s\S]*?\*\/|r?(?:"""[\s\S]*?"""|'''[\s\S]*?'''|"(?:\\.|[^"\\\r\n])*"|'(?:\\.|[^'\\\r\n])*')|\b(?:0[xX][a-fA-F0-9]+|\d+(?:\.\d+)?)\b|[a-zA-Z_$][\w$]*/g;
  let index = 0;
  let output = '';
  for (const token of source.matchAll(tokens)) {
    const value = token[0];
    let type = '';
    if (value[0] === '/') type = 'comment';
    else if (/^r?['"]/.test(value)) type = 'string';
    else if (/^\d/.test(value)) type = 'number';
    else if (words.has(value)) type = 'key';
    else if (/^_?[A-Z]/.test(value)) type = 'class';
    output += escapeHtml(source.slice(index, token.index));
    output += type ? '<span class="tok-' + type + '">' + escapeHtml(value) + '</span>' : escapeHtml(value);
    index = token.index + value.length;
  }
  return output + escapeHtml(source.slice(index));
}

export function createEditor(root, onInput) {
  root.innerHTML = '<div class="editor-surface"><div class="line-gutter"><pre class="line-numbers" aria-hidden="true"></pre></div>' +
    '<div class="code-area"><pre class="highlight" aria-hidden="true"></pre>' +
    '<textarea class="editor-input" aria-label="Source code" wrap="off" spellcheck="false" autocomplete="off" autocapitalize="off"></textarea></div></div>';
  const input = root.querySelector('textarea');
  const highlight = root.querySelector('.highlight');
  const numbers = root.querySelector('.line-numbers');
  let language = 'dart';
  let frame = 0;
  let lineCount = 0;
  function scroll() {
    highlight.style.transform = 'translate(' + (-input.scrollLeft) + 'px,' + (-input.scrollTop) + 'px)';
    numbers.style.transform = 'translateY(' + (-input.scrollTop) + 'px)';
  }
  function paint() {
    const content = input.value;
    highlight.innerHTML = (language === 'dart' ? highlightDart(content) : escapeHtml(content)) + '\n';
    const count = Math.min(22000, content.split('\n').length);
    if (count !== lineCount) {
      numbers.textContent = Array.from({ length: count }, (_, i) => i + 1).join('\n') + '\n';
      lineCount = count;
    }
    scroll();
  }
  function changed() {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(paint);
    onInput(input.value);
  }
  input.addEventListener('input', changed);
  input.addEventListener('scroll', scroll, { passive: true });
  input.addEventListener('keydown', event => {
    if (event.key === 'Tab' && !event.shiftKey && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      input.setRangeText('  ', input.selectionStart, input.selectionEnd, 'end');
      changed();
    }
  });
  return {
    set(content, type, preservePosition = false) {
      const pos = input.selectionStart;
      const { scrollLeft, scrollTop } = input;
      language = type;
      input.setAttribute('aria-label', type === 'markdown' ? 'App plan' : 'Source code');
      input.value = content;
      if (preservePosition) {
        input.setSelectionRange(pos, pos);
        input.scrollTop = scrollTop;
        input.scrollLeft = scrollLeft;
      } else {
        input.setSelectionRange(0, 0);
        input.scrollTop = 0;
        input.scrollLeft = 0;
      }
      paint();
    },
    goTo(line, index) {
      input.focus();
      input.setSelectionRange(index, index);
      input.scrollTop = Math.max(0, (line - 4) * 22);
      scroll();
    },
  };
}
