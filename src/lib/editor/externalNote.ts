import { markdownLanguage } from '@codemirror/lang-markdown';
import { splitSnapshot } from '../utils/history-comparison.ts';
import { createEditorMarkdownParser, displayMathRanges, inlineMathRanges, legacyLinkRanges } from './markdownParser.ts';

export interface ExternalNoteReference {
  from: number;
  to: number;
  href: string;
  html: boolean;
  pdf?: boolean;
}

const markdown = createEditorMarkdownParser();
// The editor never treats indented text as a code block, so an indented image is still an image here.
const editorSyntax = (markdownLanguage.parser as unknown as {
  configure(spec: { remove: string[] }): typeof markdownLanguage.parser;
}).configure({ remove: ['IndentedCode'] });

/** Byte ranges for local attachment destinations, without reserializing the user's Markdown. */
export function externalNoteReferences(raw: string): ExternalNoteReference[] {
  const { body, metadata } = splitSnapshot(raw);
  const spans: ExternalNoteReference[] = [];
  const seen = new Set<string>();
  const legacy = legacyLinkRanges(body);
  const legacyByStart = new Map(legacy.map(range => [range.from, range]));
  const math = [...displayMathRanges(body), ...inlineMathRanges(body)];
  const environment: { references?: Record<string, { href: string }> } = {};
  markdown.parse(body, environment);
  const tree = editorSyntax.parse(body);
  const usedLabels = new Set<string>();
  const label = (node: { from: number; to: number }) => markdown.utils.normalizeReference(body.slice(node.from + 1, node.to - 1));
  tree.iterate({ enter(node) {
    if (!['Link', 'Image'].includes(node.name) || node.node.getChild('URL') || body[node.to] === '(' || math.some(range => node.from >= range.from && node.from < range.to)) return;
    for (let parent = node.node.parent; parent; parent = parent.parent) if (parent.name === 'Image') return false;
    const explicit = node.node.getChild('LinkLabel');
    let key = explicit ? label(explicit) : '';
    if (!key) {
      const closing = node.node.getChildren('LinkMark').find(mark => body.slice(mark.from, mark.to) === ']');
      if (closing) key = markdown.utils.normalizeReference(body.slice(node.from + (node.name === 'Image' ? 2 : 1), closing.from));
    }
    if (environment.references?.[key]) usedLabels.add(key);
  } });
  const selectedDefinitions = new Set<string>();
  const add = (from: number, to: number, html = false, pdf = false) => {
    if (math.some(range => from >= range.from && from < range.to)) return;
    const href = markdown.utils.unescapeAll(body.slice(from, to));
    let path = href.split(/[?#]/, 1)[0];
    try { path = decodeURIComponent(path); } catch { /* Native validation handles malformed escapes. */ }
    if (!path || /^[a-z][a-z\d+.-]*:/i.test(path) || /^[\\/]/.test(path) || /\.md$/i.test(path)) return;
    const key = `${from}:${to}`;
    if (seen.has(key)) return;
    seen.add(key);
    spans.push({ from: metadata.length + from, to: metadata.length + to, href, html, pdf });
  };
  tree.iterate({
    enter(node) {
      // Links and HTML inside image alt text are labels, not additional attachments.
      let direct = true;
      for (let ancestor = node.node.parent; ancestor; ancestor = ancestor.parent) {
        if (ancestor.name === 'Image' && !(node.name === 'URL' && direct)) return false;
        direct = false;
      }
      if (node.name === 'URL' && ['Image', 'Link', 'LinkReference'].includes(node.node.parent?.name ?? '')) {
        const angle = body[node.from] === '<' && body[node.to - 1] === '>';
        const from = node.from + (angle ? 1 : 0);
        if (node.node.parent?.name === 'LinkReference') {
          const definition = node.node.parent.getChild('LinkLabel');
          if (!definition) return;
          const key = label(definition);
          const href = markdown.normalizeLink(markdown.utils.unescapeAll(body.slice(from, node.to - (angle ? 1 : 0))));
          if (!usedLabels.has(key) || selectedDefinitions.has(key) || environment.references?.[key]?.href !== href) return;
          selectedDefinitions.add(key);
        }
        const compatible = legacyByStart.get(from);
        add(from, compatible?.to ?? node.to - (angle ? 1 : 0));
      } else if (node.name === 'Paragraph' || /Heading[1-6]$/.test(node.name)) {
        for (const range of legacy) {
          if (range.from >= node.from && range.to <= node.to) add(range.from, range.to);
        }
      } else if (node.name === 'HTMLTag' || node.name === 'HTMLBlock') {
        const html = body.slice(node.from, node.to);
        // Skip comments and literal HTML code, not just Markdown fences.
        const tags = /<!--[\s\S]*?(?:-->|$)|<(script|style|textarea|pre|code)\b[^>]*>[\s\S]*?(?:<\/\1\s*>|$)|<(img|a|div)\b(?:"[^"]*"|'[^']*'|[^'">])*>/gi;
        for (const tag of html.matchAll(tags)) {
          if (!tag[2]) continue;
          const attributes = /(?:^|\s)([\w:-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/gi;
          for (const attribute of tag[0].matchAll(attributes)) {
            const name = attribute[1].toLowerCase();
            const element = tag[2].toLowerCase();
            if (!(element === 'img' && name === 'src') && !(element === 'a' && name === 'href') && !(element === 'div' && name === 'data-pdf-src')) continue;
            const value = attribute[2] ?? attribute[3] ?? attribute[4];
            const quoted = attribute[2] !== undefined || attribute[3] !== undefined;
            const start = node.from + tag.index! + attribute.index! + attribute[0].length - value.length - (quoted ? 1 : 0);
            add(start, start + value.length, true, name === 'data-pdf-src');
          }
        }
      }
    },
  });
  spans.sort((a, b) => a.from - b.from);
  const encoder = new TextEncoder();
  let previous = 0;
  let bytes = 0;
  return spans.map(span => {
    bytes += encoder.encode(raw.slice(previous, span.from)).length;
    const from = bytes;
    bytes += encoder.encode(raw.slice(span.from, span.to)).length;
    previous = span.to;
    return { ...span, from, to: bytes };
  });
}
