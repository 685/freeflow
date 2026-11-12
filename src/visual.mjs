import { Problem } from './files.mjs';

const alignments = ['start', 'end', 'center', 'spaceBetween', 'spaceAround', 'spaceEvenly'];
const crossAlignments = ['start', 'end', 'center', 'stretch', 'baseline'];
const textAlignments = ['left', 'right', 'center', 'justify', 'start', 'end'];
const sizes = ['min', 'max'];
const fontWeights = ['w100', 'w200', 'w300', 'w400', 'w500', 'w600', 'w700', 'w800', 'w900'];
const field = (name, type, extra = {}) => ({ name, type, ...extra });
const enumField = (name, prefix, options, extra = {}) => field(name, 'select', { prefix, options, ...extra });
const number = name => field(name, 'number');
const color = name => field(name, 'color');
const padding = name => field(name, 'spacing');
const layoutFields = [
  enumField('mainAxisAlignment', 'MainAxisAlignment', alignments),
  enumField('crossAxisAlignment', 'CrossAxisAlignment', crossAlignments),
  enumField('mainAxisSize', 'MainAxisSize', sizes),
  number('spacing'),
];
const textFields = [
  field('text', 'string', { positional: true }),
  enumField('textAlign', 'TextAlign', textAlignments),
  field('fontSize', 'number', { nest: ['style', 'TextStyle'] }),
  field('color', 'color', { nest: ['style', 'TextStyle'] }),
  enumField('fontWeight', 'FontWeight', fontWeights, { nest: ['style', 'TextStyle'] }),
];
const spec = (slots = {}, fields = []) => ({ slots, fields });
const child = { child: false };
const children = { children: true };
const catalog = new Map(Object.entries({
  MaterialApp: spec({ home: false }, [field('title', 'string'), field('debugShowCheckedModeBanner', 'boolean')]),
  Scaffold: spec({ appBar: false, body: false, floatingActionButton: false, bottomNavigationBar: false }, [color('backgroundColor')]),
  AppBar: spec({ title: false, leading: false, actions: true }, [color('backgroundColor'), color('foregroundColor'), field('centerTitle', 'boolean')]),
  Text: spec({}, textFields),
  Icon: spec({}, [number('size'), color('color')]),
  Row: spec(children, layoutFields),
  Column: spec(children, layoutFields),
  Wrap: spec(children, [number('spacing'), number('runSpacing')]),
  Stack: spec(children),
  ListView: spec(children, [padding('padding'), field('shrinkWrap', 'boolean'), field('reverse', 'boolean')]),
  GridView: spec(children, [padding('padding')]),
  SafeArea: spec(child, ['left', 'top', 'right', 'bottom'].map(name => field(name, 'boolean'))),
  Center: spec(child),
  Align: spec(child),
  Padding: spec(child, [padding('padding')]),
  SizedBox: spec(child, [number('width'), number('height')]),
  Expanded: spec(child, [field('flex', 'integer')]),
  Flexible: spec(child, [field('flex', 'integer')]),
  Spacer: spec({}, [field('flex', 'integer')]),
  Container: spec(child, [number('width'), number('height'), color('color'), padding('padding'), padding('margin')]),
  Card: spec(child, [color('color'), number('elevation'), padding('margin')]),
  Material: spec(child, [color('color'), number('elevation')]),
  DecoratedBox: spec(child),
  ConstrainedBox: spec(child),
  SingleChildScrollView: spec(child, [padding('padding'), field('reverse', 'boolean')]),
  AspectRatio: spec(child, [field('aspectRatio', 'number', { min: 0.001 })]),
  ClipRRect: spec(child),
  Divider: spec({}, [color('color'), number('height'), number('thickness')]),
  LinearProgressIndicator: spec({}, [color('color'), color('backgroundColor'), number('minHeight'), field('value', 'number', { max: 1 })]),
  ListTile: spec({ leading: false, title: false, subtitle: false, trailing: false }, [field('dense', 'boolean')]),
  CheckboxListTile: spec({ title: false, subtitle: false, secondary: false }, [color('activeColor'), field('dense', 'boolean')]),
  ElevatedButton: spec(child),
  FilledButton: spec(child),
  OutlinedButton: spec(child),
  TextButton: spec(child),
  IconButton: spec({ icon: false }, [field('tooltip', 'string')]),
  FloatingActionButton: spec(child, [field('tooltip', 'string'), color('backgroundColor')]),
  TextField: spec({}, [field('maxLines', 'integer'), field('obscureText', 'boolean'), field('enabled', 'boolean')]),
  Image: spec({}, [number('width'), number('height')]),
  GestureDetector: spec(child),
  InkWell: spec(child),
  NavigationBar: spec({ destinations: true }, [color('backgroundColor'), number('height')]),
  NavigationDestination: spec({ icon: false, selectedIcon: false }, [field('label', 'string')]),
}));

const palette = {
  Text: "Text('New text')",
  Icon: 'Icon(Icons.star_outline)',
  SizedBox: 'SizedBox(height: 16)',
  Padding: "Padding(padding: EdgeInsets.all(16), child: Text('New text'))",
  Center: "Center(child: Text('New text'))",
  Column: "Column(children: [Text('New text')])",
  Row: "Row(children: [Text('New text')])",
  Card: "Card(child: Padding(padding: EdgeInsets.all(16), child: Text('Card')))",
  Divider: 'Divider()',
  FilledButton: "FilledButton(onPressed: null, child: Text('Button'))",
  TextField: "TextField(decoration: InputDecoration(labelText: 'Label'))",
  ListTile: "ListTile(title: Text('List item'))",
  AppBar: "AppBar(title: Text('App'))",
  NavigationDestination: "NavigationDestination(icon: Icon(Icons.home_outlined), label: 'Home')",
};

function slotChoices(node, slot) {
  if (node.type === 'Scaffold' && slot === 'appBar') return ['AppBar'];
  if (node.type === 'NavigationBar' && slot === 'destinations') return ['NavigationDestination'];
  return Object.keys(palette).filter(name => name !== 'AppBar' && name !== 'NavigationDestination');
}

const wrappers = {
  Padding: 'Padding(padding: const EdgeInsets.all(16), child: ',
  Center: 'Center(child: ',
  SizedBox: 'SizedBox(width: 240, child: ',
  Card: 'Card(child: ',
};

function commentEnd(source, start) {
  if (source.startsWith('//', start)) {
    const newline = source.indexOf('\n', start + 2);
    return newline < 0 ? source.length : newline;
  }
  let depth = 1;
  let i = start + 2;
  while (i < source.length && depth) {
    if (source.startsWith('/*', i)) {
      depth++;
      i += 2;
    } else if (source.startsWith('*/', i)) {
      depth--;
      i += 2;
    } else i++;
  }
  return i;
}

function stringEnd(source, start, recursion = 0) {
  if (recursion > 100) throw new Problem(422, 'This Dart expression is too deeply nested for the visual editor.');
  const raw = source[start] === 'r';
  const quoteAt = start + Number(raw);
  const quote = source[quoteAt];
  const delimiter = source.startsWith(quote.repeat(3), quoteAt) ? quote.repeat(3) : quote;
  let i = quoteAt + delimiter.length;
  while (i < source.length) {
    if (source.startsWith(delimiter, i)) return i + delimiter.length;
    if (!raw && source[i] === '\\') {
      i += 2;
    } else if (!raw && source.startsWith('\${', i)) {
      i += 2;
      let depth = 1;
      while (i < source.length && depth) {
        if (source.startsWith('//', i) || source.startsWith('/*', i)) i = commentEnd(source, i);
        else if (isStringStart(source, i)) i = stringEnd(source, i, recursion + 1);
        else {
          if (source[i] === '{') depth++;
          if (source[i] === '}') depth--;
          i++;
        }
      }
    } else i++;
  }
  return source.length;
}

function isStringStart(source, index) {
  const first = source[index];
  return first === "'" || first === '"' || (first === 'r' && (source[index + 1] === "'" || source[index + 1] === '"'));
}

function tokenize(source) {
  const tokens = [];
  const stack = [];
  const pairs = { ')': '(', ']': '[', '}': '{' };
  const name = /[A-Za-z_$][\w$]*/y;
  let i = 0;
  while (i < source.length) {
    if (/\s/.test(source[i])) {
      i++;
      continue;
    }
    if (source.startsWith('//', i) || source.startsWith('/*', i)) {
      i = commentEnd(source, i);
      continue;
    }
    const start = i;
    if (isStringStart(source, i)) i = stringEnd(source, i);
    else {
      name.lastIndex = i;
      const match = name.exec(source);
      i += match ? match[0].length : 1;
    }
    const value = source.slice(start, i);
    const token = { value, start, end: i };
    const index = tokens.length;
    if (value === '(' || value === '[' || value === '{') stack.push(index);
    else if (Object.hasOwn(pairs, value)) {
      const prior = stack.pop();
      if (tokens[prior]?.value === pairs[value]) tokens[prior].close = index;
    }
    tokens.push(token);
  }
  return tokens;
}

// An angle-bracket group followed by a collection literal or invocation is
// a type argument list. Treat other less-than operators as Dart expressions.
function typeArgumentEnd(tokens, start, limit) {
  if (tokens[start].value !== '<') return start;
  let depth = 1;
  for (let i = start + 1; i < Math.min(limit, start + 64); i++) {
    const value = tokens[i].value;
    if (value === '<') depth++;
    else if (value === '>' && --depth === 0) {
      return ['[', '{', '(', '.'].includes(tokens[i + 1]?.value) ? i : start;
    } else if (!/^(?:[\w$.]+|[,?>])$/.test(value)) return start;
  }
  return start;
}

function segments(tokens, open, close) {
  const result = [];
  let first = open + 1;
  for (let i = first; i <= close; i++) {
    if (i === close || tokens[i].value === ',') {
      if (i > first) result.push({ first, last: i - 1, comma: i < close ? i : null });
      first = i + 1;
      continue;
    }
    i = tokens[i].close ?? typeArgumentEnd(tokens, i, close);
  }
  return result;
}

function argsFor(tokens, open) {
  return segments(tokens, open, tokens[open].close).map(segment => {
    const isNamed = tokens[segment.first + 1]?.value === ':' && segment.last > segment.first + 1;
    return {
      ...segment,
      name: isNamed ? tokens[segment.first].value : null,
      expression: isNamed ? segment.first + 2 : segment.first,
    };
  });
}

function decodeLiteral(expression, item) {
  if (item.type === 'string') {
    const quote = expression[0];
    if ((quote !== "'" && quote !== '"') || expression.startsWith(quote.repeat(3)) || stringEnd(expression, 0) !== expression.length) return undefined;
    const raw = expression.slice(1, -1);
    if (/(?:^|[^\\])(?:\\\\)*\$/.test(raw) || /\\(?!['"\\$nrt])/.test(raw)) return undefined;
    return raw.replace(/\\(['"\\$nrt])/g, (_, key) => ({ n: '\n', r: '\r', t: '\t' })[key] ?? key);
  }
  if (item.type === 'boolean') return ['true', 'false'].includes(expression) ? expression === 'true' : undefined;
  if (item.type === 'select') {
    const value = expression.slice(item.prefix.length + 1);
    return expression.startsWith(item.prefix + '.') && item.options.includes(value) ? value : undefined;
  }
  if (item.type === 'color') {
    const rgb = expression.match(/^(?:const\s+)?Color\(0x[fF]{2}([a-fA-F0-9]{6})\)$/)?.[1];
    if (rgb) return '#' + rgb.toLowerCase();
    return undefined;
  }
  if (item.type === 'spacing') {
    const value = expression.match(/^(?:const\s+)?EdgeInsets\.all\(\s*(\d+(?:\.\d+)?)\s*\)$/)?.[1];
    return value === undefined ? undefined : Number(value);
  }
  return /^-?(?:\d+(?:\.\d+)?|\.\d+)$/.test(expression) ? Number(expression) : undefined;
}

function encodeLiteral(value, item) {
  if (item.type === 'string' && typeof value === 'string' && value.length <= 4000) {
    return "'" + value.replace(/[\\'$\n\r\t]/g, char => ({ '\n': '\\n', '\r': '\\r', '\t': '\\t' })[char] || '\\' + char) + "'";
  }
  if (item.type === 'boolean' && typeof value === 'boolean') return String(value);
  if (item.type === 'select' && item.options.includes(value)) return item.prefix + '.' + value;
  if (item.type === 'color' && typeof value === 'string' && /^#[a-f0-9]{6}$/i.test(value)) return 'const Color(0xff' + value.slice(1).toLowerCase() + ')';
  if (['integer', 'number', 'spacing'].includes(item.type) && typeof value === 'number' &&
      Number.isFinite(value) && value >= (item.min ?? (item.type === 'integer' ? 1 : 0)) && value <= (item.max ?? 10000) &&
      (item.type !== 'integer' || Number.isInteger(value))) {
    return item.type === 'spacing' ? 'const EdgeInsets.all(' + value + ')' : String(value);
  }
  throw new Problem(422, 'Enter a valid value for ' + item.name + '.');
}

function listFor(tokens, segment) {
  if (!segment) return null;
  let open = segment.expression;
  if (tokens[open].value === 'const') open++;
  if (tokens[open]?.value === '<') {
    const closeType = typeArgumentEnd(tokens, open, segment.last + 1);
    if (closeType > open) open = closeType + 1;
  }
  if (tokens[open]?.value !== '[' || tokens[open].close !== segment.last) return null;
  return { open, close: segment.last, items: segments(tokens, open, segment.last) };
}

function construct(tokens, start, end, name) {
  let at = start;
  if (tokens[at].value === 'const' || tokens[at].value === 'new') at++;
  const open = at + 1;
  return tokens[at]?.value === name && tokens[open]?.value === '(' && tokens[open].close === end
    ? { open, close: end, args: argsFor(tokens, open) } : null;
}

function inspect(source) {
  const tokens = tokenize(source);
  const custom = new Set();
  for (let i = 0; i < tokens.length - 3; i++) {
    if (tokens[i].value === 'class' && tokens[i + 2].value === 'extends' &&
        ['StatelessWidget', 'StatefulWidget', 'ConsumerWidget', 'HookWidget'].includes(tokens[i + 3].value)) custom.add(tokens[i + 1].value);
  }
  const nodes = [];
  const parents = [];
  const lineBreaks = Array.from(source.matchAll(/\n/g), match => match.index);
  let line = 0;
  for (let i = 0; i < tokens.length - 1; i++) {
    const type = tokens[i].value;
    if (!catalog.has(type) && !custom.has(type)) continue;
    if (tokens[i - 1]?.value === 'class' || tokens[i - 1]?.value === 'factory') continue;
    let open = i + 1;
    let constructor = '';
    if (tokens[open]?.value === '.' && tokens[open + 2]?.value === '(') {
      constructor = '.' + tokens[open + 1].value;
      open += 2;
    }
    if (tokens[open]?.value !== '(' || tokens[open].close === undefined) continue;
    const close = tokens[open].close;
    if ([':', '{', '=>'].includes(tokens[close + 1]?.value) || (custom.has(type) && tokens[open + 1]?.value === '{')) continue;
    const start = ['const', 'new'].includes(tokens[i - 1]?.value) ? i - 1 : i;
    while (parents.length && parents.at(-1).close < open) parents.pop();
    while (line < lineBreaks.length && lineBreaks[line] < tokens[start].start) line++;
    const parent = parents.at(-1);
    const node = {
      id: String(tokens[start].start), type, constructor, open, close, start,
      line: line + 1, parent: parent?.id ?? null, args: argsFor(tokens, open),
      spec: catalog.get(type) || spec(),
    };
    nodes.push(node);
    parents.push(node);
  }
  return { tokens, nodes };
}

function propertyContext(node, item, tokens) {
  if (item.nest) {
    const outer = node.args.find(arg => arg.name === item.nest[0]);
    const container = outer && construct(tokens, outer.expression, outer.last, item.nest[1]);
    return {
      outer, container,
      arg: container?.args.find(arg => arg.name === item.name),
      editable: !outer || !!container,
    };
  }
  const arg = node.args.find(item.positional ? arg => arg.name === null : arg => arg.name === item.name);
  const conflict = node.type === 'Container' && item.name === 'color' && node.args.some(arg => arg.name === 'decoration');
  return { container: node, arg, editable: !conflict && (!item.positional || Boolean(arg)) };
}

function memberSlot(node, parent, tokens) {
  if (!parent) return null;
  for (const [slot, multiple] of Object.entries(parent.spec.slots)) {
    const arg = parent.args.find(arg => arg.name === slot);
    if (!arg) continue;
    if (!multiple && arg.expression === node.start && arg.last === node.close) return { kind: 'arg', segment: arg, items: parent.args };
    const list = multiple && listFor(tokens, arg);
    const item = list && list.items.find(entry => entry.first === node.start && entry.last === node.close);
    if (item) return { kind: 'list', segment: item, items: list.items, list, slot };
  }
  return null;
}

export function outline(source) {
  const { tokens, nodes } = inspect(source);
  const byId = new Map(nodes.map(node => [node.id, node]));
  return {
    palette: Object.keys(palette),
    wrappers: Object.keys(wrappers),
    nodes: nodes.map(node => {
      const properties = node.spec.fields.map(item => {
        const context = propertyContext(node, item, tokens);
        const expression = context.arg && source.slice(tokens[context.arg.expression].start, tokens[context.arg.last].end);
        const value = expression === undefined ? undefined : decodeLiteral(expression, item);
        // Named color constants can be replaced with an explicit Color literal.
        const colorConstant = item.type === 'color' && expression && /^Colors\.[\w]+$/.test(expression);
        return {
          name: item.name, type: item.type, options: item.options, value,
          expression: expression?.slice(0, 200),
          editable: context.editable && (expression === undefined || value !== undefined || !!colorConstant),
        };
      });
      const slots = Object.entries(node.spec.slots).filter(([name, multiple]) => {
        const arg = node.args.find(entry => entry.name === name);
        return !arg || (multiple && listFor(tokens, arg));
      }).map(([name]) => name);
      const parent = byId.get(node.parent);
      const member = memberSlot(node, parent, tokens);
      const typedAppBar = node.type === 'AppBar' && parent?.type === 'Scaffold';
      return {
        id: node.id, type: node.type + node.constructor, line: node.line, parent: node.parent,
        label: properties.find(item => item.name === 'text' || item.name === 'label')?.value,
        slots, slotChoices: Object.fromEntries(slots.map(slot => [slot, slotChoices(node, slot)])),
        properties, removable: !!member, movable: member?.kind === 'list', listSlot: member?.slot,
        wrappable: !typedAppBar,
      };
    }),
  };
}

function replace(source, start, end, value) {
  return source.slice(0, start) + value + source.slice(end);
}

function append(source, tokens, open, close, items, value) {
  const last = items.at(-1);
  if (!last) return replace(source, tokens[open].end, tokens[open].end, value);
  if (last.comma !== null) return replace(source, tokens[last.comma].end, tokens[last.comma].end, ' ' + value + ',');
  return replace(source, tokens[last.last].end, tokens[last.last].end, ', ' + value);
}

export function editVisual(source, command) {
  if (!command || typeof command !== 'object') throw new Problem(400, 'Choose a visual edit.');
  const { tokens, nodes } = inspect(source);
  const node = nodes.find(entry => entry.id === command.id);
  if (!node) throw new Problem(409, 'Select the widget again after the source refreshes.');
  const { operation } = command;
  if (operation === 'property') {
    const item = node.spec.fields.find(entry => entry.name === command.name);
    if (!item) throw new Problem(400, 'This property is unavailable for the widget.');
    const { editable, outer, container, arg } = propertyContext(node, item, tokens);
    const expression = arg && source.slice(tokens[arg.expression].start, tokens[arg.last].end);
    if (!editable || (expression !== undefined && decodeLiteral(expression, item) === undefined &&
        !(item.type === 'color' && /^Colors\.[\w]+$/.test(expression)))) {
      throw new Problem(422, 'Edit this Dart expression in the code editor.');
    }
    const value = encodeLiteral(command.value, item);
    if (arg) return replace(source, tokens[arg.expression].start, tokens[arg.last].end, value);
    if (item.nest && !outer) {
      const [name, type] = item.nest;
      return append(source, tokens, node.open, node.close, node.args, name + ': ' + type + '(' + item.name + ': ' + value + ')');
    }
    return append(source, tokens, container.open, container.close, container.args, item.name + ': ' + value);
  }
  if (operation === 'add') {
    const slot = command.slot;
    if (!Object.hasOwn(node.spec.slots, slot) || !slotChoices(node, slot).includes(command.widget)) throw new Problem(400, 'Select an available widget and slot.');
    const value = palette[command.widget];
    const multiple = node.spec.slots[slot];
    const arg = node.args.find(entry => entry.name === slot);
    if (!arg) return append(source, tokens, node.open, node.close, node.args, slot + ': ' + (multiple ? '[' + value + ']' : value));
    const list = multiple && listFor(tokens, arg);
    if (!list) throw new Problem(422, 'The slot is filled by a Dart expression. Edit it in Code.');
    return append(source, tokens, list.open, list.close, list.items, value);
  }
  if (operation === 'wrap') {
    if (!Object.hasOwn(wrappers, command.widget)) throw new Problem(400, 'Select a supported wrapper.');
    if (node.type === 'AppBar' && nodes.some(entry => entry.id === node.parent && entry.type === 'Scaffold')) {
      throw new Problem(422, 'Scaffold.appBar requires an AppBar or another PreferredSizeWidget.');
    }
    const from = tokens[node.start].start;
    const to = tokens[node.close].end;
    const prefix = tokens[node.start].value === 'const' ? 'const ' : '';
    return replace(source, from, to, prefix + wrappers[command.widget] + source.slice(from, to) + ')');
  }
  const parent = nodes.find(entry => entry.id === node.parent);
  const member = memberSlot(node, parent, tokens);
  if (operation === 'remove' && member) {
    const { segment, items } = member;
    const previous = items[items.indexOf(segment) - 1];
    const start = segment.comma === null && previous ? tokens[previous.comma].start : tokens[segment.first].start;
    const end = segment.comma === null ? tokens[segment.last].end : tokens[segment.comma].end;
    return replace(source, start, end, '');
  }
  if (operation === 'move' && member?.kind === 'list') {
    const other = nodes.find(entry => entry.id === command.target);
    const to = other && memberSlot(other, parent, tokens);
    if (to?.list?.open !== member.list.open || !['before', 'after'].includes(command.side)) {
      throw new Problem(422, 'Reorder widgets within the same children list.');
    }
    const items = member.items;
    const fromIndex = items.indexOf(member.segment);
    const otherIndex = items.findIndex(item => item.first === to.segment.first);
    if (fromIndex === otherIndex) return source;
    const start = Math.min(fromIndex, otherIndex);
    const end = Math.max(fromIndex, otherIndex);
    const affected = items.slice(start, end + 1);
    const values = affected.map(item => source.slice(tokens[item.first].start, tokens[item.last].end));
    const [moved] = values.splice(fromIndex - start, 1);
    values.splice(otherIndex - start - Number(fromIndex < otherIndex) + Number(command.side === 'after'), 0, moved);
    let result = source;
    for (let i = affected.length - 1; i >= 0; i--) {
      result = replace(result, tokens[affected[i].first].start, tokens[affected[i].last].end, values[i]);
    }
    return result;
  }
  throw new Problem(422, 'That change needs to be made in the code editor.');
}
