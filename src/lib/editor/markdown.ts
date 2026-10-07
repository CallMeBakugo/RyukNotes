import type { Mark, Node as ProseMirrorNode } from '@tiptap/pm/model';

type AssetSourceNormalizer = (source: string) => string;

type MarkdownMark = {
  key: string;
  priority: number;
  open: string;
  close: string;
};

const identityAssetSource: AssetSourceNormalizer = (source) => source;

function backtickDelimiter(text: string, minimum: number): string {
  let length = minimum;
  for (const match of text.matchAll(/`+/g)) {
    length = Math.max(length, match[0].length + 1);
  }
  return '`'.repeat(length);
}

export interface MarkdownBlock {
  text: string;
  isImage: boolean;
  /** An empty paragraph, which is kept as a marker line so its vertical space round-trips. */
  isEmpty?: boolean;
}

const EMPTY_PARAGRAPH_MARKER = '<!-- -->';

/**
 * Join serialized top-level blocks into a note body. Blocks that already end in a blank line
 * (lists) get no extra separator, and an empty paragraph directly before an image is written as
 * the blank line the parser turns back into that paragraph, not as a marker glued to the image.
 */
export function joinMarkdownBlocks(blocks: readonly MarkdownBlock[]): string {
  const entries = [...blocks];
  while (entries.length > 0 && entries[entries.length - 1].isEmpty) entries.pop();
  let result = '';
  entries.forEach((entry, index) => {
    if (entry.isEmpty && entries[index + 1]?.isImage) {
      if (index > 0 && !result.endsWith('\n\n')) result += result.endsWith('\n') ? '\n' : '\n\n';
      return;
    }
    const text = entry.isEmpty ? EMPTY_PARAGRAPH_MARKER : entry.text;
    // Images follow their predecessor directly; every other block needs a blank line before it.
    result += index === 0 ? text : (entry.isImage || result.endsWith('\n\n') ? '' : '\n') + text;
  });
  return result.trim() + '\n';
}

export function serializeCodeBlock(code: string, language: string = ''): string {
  const fence = backtickDelimiter(code, 3);
  return `${fence}${language}\n${code}\n${fence}\n`;
}

function serializeCodeSpan(text: string): string {
  const delimiter = backtickDelimiter(text, 1);
  const needsPadding = text.startsWith('`') || text.endsWith('`')
    || ((text.startsWith(' ') || text.endsWith(' ')) && /[^ ]/.test(text));
  const padding = needsPadding ? ' ' : '';
  return `${delimiter}${padding}${text}${padding}${delimiter}`;
}

function serializeInlineMath(tex: string, forceEncoded: boolean): string {
  if (forceEncoded || !tex || /[\r\n$]/.test(tex) || /^\s|\s$/.test(tex) || /(?:^|[^\\])(?:\\\\)*\\$/.test(tex)) {
    return `<span data-math-inline="${encodeURIComponent(tex)}" class="math-inline"></span>`;
  }
  return `$${tex}$`;
}

function stringAttr(mark: Mark, name: string): string {
  const value = mark.attrs?.[name];
  return typeof value === 'string' ? value : '';
}

function markdownMark(mark: Mark, text: string): MarkdownMark | null {
  switch (mark.type.name) {
    case 'bold':
      return { key: 'bold', priority: 10, open: '**', close: '**' };
    case 'italic':
      return { key: 'italic', priority: 20, open: '*', close: '*' };
    case 'strike':
      return { key: 'strike', priority: 30, open: '~~', close: '~~' };
    case 'underline':
      return { key: 'underline', priority: 40, open: '<u>', close: '</u>' };
    case 'subscript':
      return { key: 'subscript', priority: 50, open: '~', close: '~' };
    case 'superscript':
      return { key: 'superscript', priority: 60, open: '^', close: '^' };
    case 'highlight': {
      const color = stringAttr(mark, 'color');
      return color
        ? {
            key: `highlight:${color}`,
            priority: 70,
            open: `<mark data-color="${color}">`,
            close: '</mark>',
          }
        : { key: 'highlight', priority: 70, open: '==', close: '==' };
    }
    case 'textStyle': {
      const color = stringAttr(mark, 'color');
      return color
        ? {
            key: `textStyle:${color}`,
            priority: 80,
            open: `<span style="color: ${color}">`,
            close: '</span>',
          }
        : null;
    }
    case 'link': {
      const href = stringAttr(mark, 'href');
      return {
        key: `link:${href}`,
        priority: 100,
        open: '[',
        close: `](${href})`,
      };
    }
    case 'wikiLink': {
      const title = stringAttr(mark, 'title') || text;
      const aliased = mark.attrs?.aliased === true || title !== text;
      return {
        key: `wikiLink:${title}:${aliased}`,
        priority: 110,
        open: aliased ? `[[${title}|` : '[[',
        close: ']]',
      };
    }
    default:
      return null;
  }
}

function marksForText(node: ProseMirrorNode, code = false): MarkdownMark[] {
  return node.marks
    .filter((mark) => !code || mark.type.name === 'link' || mark.type.name === 'wikiLink')
    .map((mark) => markdownMark(mark, node.text || ''))
    .filter((mark): mark is MarkdownMark => mark !== null)
    .sort((left, right) => left.priority - right.priority || left.key.localeCompare(right.key));
}

function commonMarkCount(active: MarkdownMark[], next: MarkdownMark[]): number {
  const length = Math.min(active.length, next.length);
  let index = 0;
  while (index < length && active[index].key === next[index].key) index += 1;
  return index;
}

export function serializeInlineMarkdown(
  node: ProseMirrorNode,
  normalizeAssetSource: AssetSourceNormalizer = identityAssetSource,
): string {
  let result = '';
  let activeMarks: MarkdownMark[] = [];

  const transitionMarks = (nextMarks: MarkdownMark[]) => {
    const shared = commonMarkCount(activeMarks, nextMarks);
    for (let index = activeMarks.length - 1; index >= shared; index -= 1) {
      result += activeMarks[index].close;
    }
    for (let index = shared; index < nextMarks.length; index += 1) {
      result += nextMarks[index].open;
    }
    activeMarks = nextMarks;
  };

  for (let index = 0; index < node.childCount; index += 1) {
    const child = node.child(index);
    if (child.isText) {
      const isCode = child.marks.some((mark) => mark.type.name === 'code');
      const marks = marksForText(child, isCode);
      transitionMarks(marks);
      let text = child.text || '';
      if (isCode) {
        while (index + 1 < node.childCount) {
          const next = node.child(index + 1);
          if (!next.isText || !next.marks.some((mark) => mark.type.name === 'code')) break;
          const nextMarks = marksForText(next, true);
          if (marks.length !== nextMarks.length || commonMarkCount(marks, nextMarks) !== marks.length) break;
          text += next.text || '';
          index += 1;
        }
        result += serializeCodeSpan(text);
      } else {
        if (index === 0) {
          text = text.replace(/^[\t\u2003]+/, (whitespace) => '&emsp;'.repeat(whitespace.length));
        }
        text = text.replace(/\\*\$/g, (match) => '\\'.repeat((match.length - 1) * 2 + 1) + '$');
        if (index + 1 < node.childCount && node.child(index + 1).type.name === 'mathInline') {
          text = text.replace(/\\+$/, (backslashes) => backslashes + backslashes);
        }
        result += text;
      }
      continue;
    }

    transitionMarks([]);
    if (child.type.name === 'image') {
      const source = normalizeAssetSource(child.attrs.src || '');
      if (!source) continue;
      const alt = child.attrs.alt || '';
      const size = child.attrs['data-size'] || child.attrs.size || 'full';
      const sizeSuffix = size && size !== 'full' ? `|size=${size}` : '';
      if (result) result += '\n';
      result += `![${alt}${sizeSuffix}](${source})`;
    } else if (child.type.name === 'mathInline') {
      const previous = index > 0 ? node.child(index - 1) : null;
      const next = index + 1 < node.childCount ? node.child(index + 1) : null;
      const forceEncoded = previous?.type.name === 'mathInline' || next?.type.name === 'mathInline'
        || /\$$/.test(previous?.text || '') || /^[\d$]/.test(next?.text || '');
      result += serializeInlineMath(child.attrs.tex || '', forceEncoded);
    } else if (child.type.name === 'hardBreak') {
      result += '  \n';
    }
  }

  transitionMarks([]);
  return result;
}
