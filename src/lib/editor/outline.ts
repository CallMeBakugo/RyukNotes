import { markdownLanguage } from '@codemirror/lang-markdown';
import { MapMode, type ChangeDesc, type EditorState } from '@codemirror/state';
import type { Node as ProseMirrorNode } from '@tiptap/pm/model';
import type { Mapping } from '@tiptap/pm/transform';
import MarkdownIt from 'markdown-it';
import type Token from 'markdown-it/lib/token.mjs';
import markdownItMark from 'markdown-it-mark';
import markdownItSub from 'markdown-it-sub';
import markdownItSup from 'markdown-it-sup';
import { displayMathRanges } from './markdownParser.ts';

export interface OutlineHeading {
  level: number;
  text: string;
  pos: number;
  end: number;
}

const inlineMarkdown = MarkdownIt({ html: true, linkify: false, breaks: false })
  .use(markdownItMark)
  .use(markdownItSub)
  .use(markdownItSup);

function inlineText(tokens: readonly Token[]): string {
  let text = '';
  for (const token of tokens) {
    if (token.children) text += inlineText(token.children);
    else if (token.type === 'text' || token.type === 'code_inline') text += token.content;
    else if (token.type === 'softbreak' || token.type === 'hardbreak' ||
      (token.type === 'html_inline' && /^<br\s*\/?\s*>$/i.test(token.content))) text += ' ';
  }
  return text;
}

export function documentHeadings(doc: ProseMirrorNode): OutlineHeading[] {
  const headings: OutlineHeading[] = [];
  doc.descendants((node, pos) => {
    if (node.type.name !== 'heading') return;
    headings.push({ level: node.attrs.level, text: node.textContent, pos, end: pos + node.nodeSize });
    return false;
  });
  return headings;
}

/** Match the rich editor's closed $$ blocks without shifting source coordinates. */
function maskDisplayMath(markdown: string): string {
  const ranges = displayMathRanges(markdown);
  if (!ranges.length) return markdown;
  const parts: string[] = [];
  let copied = 0;
  for (const { from, to, prefix } of ranges) {
    parts.push(markdown.slice(copied, from), prefix, markdown.slice(from + prefix.length, to).replace(/[^\n]/g, ' '));
    copied = to;
  }
  parts.push(markdown.slice(copied));
  return parts.join('');
}

/** Markdown headings only: raw HTML heading blocks are not interpreted as headings. */
export function sourceHeadings(state: EditorState): OutlineHeading[] {
  const headings: OutlineHeading[] = [];
  // Parse the complete document, independent of CodeMirror's viewport/time-limited syntax tree.
  const markdown = state.doc.toString();
  const parsedMarkdown = maskDisplayMath(markdown);
  const tree = markdownLanguage.parser.parse(parsedMarkdown);
  const environment = {};
  // Resolve reference labels only when the document can contain link definitions.
  if (parsedMarkdown.includes(']:')) inlineMarkdown.parse(parsedMarkdown, environment);
  tree.iterate({
    enter(node) {
      if (!/^(?:ATX|Setext)Heading[1-6]$/.test(node.name)) return;
      const setext = node.name.startsWith('SetextHeading');
      const cursor = node.node.cursor();
      let from = node.from;
      let label = '';
      do {
        if (cursor.name !== 'HeaderMark' && cursor.name !== 'QuoteMark' && cursor.name !== 'ListMark') continue;
        // Setext's underline and its enclosing quote/list prefix are not label content.
        const to = setext && cursor.name === 'HeaderMark'
          ? state.doc.lineAt(cursor.from).from : cursor.from;
        label += state.sliceDoc(from, Math.max(from, to));
        from = cursor.to;
      } while (cursor.next() && cursor.from < node.to);
      if (!setext) label += state.sliceDoc(from, node.to);
      headings.push({
        level: Number(node.name.at(-1)),
        text: inlineText(inlineMarkdown.parseInline(label, environment)).replace(/\s+/gu, ' ').trim(),
        pos: node.from,
        end: node.to,
      });
      return false;
    },
  });
  return headings;
}

/** Headings are in document order; body content remains associated with the preceding heading. */
export function activeHeadingIndex(headings: readonly OutlineHeading[], pos: number): number {
  let low = 0;
  let high = headings.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (headings[middle].pos <= pos) low = middle + 1;
    else high = middle;
  }
  return low - 1;
}

/** Temporary anchors only; validate against fresh extraction before navigating. */
export function mapSourceHeadings(headings: readonly OutlineHeading[], changes: ChangeDesc): OutlineHeading[] {
  const mapped: OutlineHeading[] = [];
  for (const heading of headings) {
    const pos = changes.mapPos(heading.pos, 1, MapMode.TrackAfter);
    const end = changes.mapPos(heading.end, -1);
    if (pos !== null && pos < end) mapped.push({ ...heading, pos, end });
  }
  return mapped;
}

/** A replaced opening token no longer identifies the original semantic heading. */
export function mapDocumentHeadings(headings: readonly OutlineHeading[], mapping: Mapping): OutlineHeading[] {
  const mapped: OutlineHeading[] = [];
  for (const heading of headings) {
    const start = mapping.mapResult(heading.pos, 1);
    const end = mapping.map(heading.end, -1);
    if (!start.deleted && start.pos < end) mapped.push({ ...heading, pos: start.pos, end });
  }
  return mapped;
}
