import { describe, it, expect } from 'vitest'
import { parseSegments, hasMermaid, renderContent } from './markdown.js'

describe('renderContent', () => {
  // XSS protection
  it('escapes HTML angle brackets', () => {
    expect(renderContent('<script>alert("xss")</script>')).toContain('&lt;script')
    expect(renderContent('<script>alert("xss")</script>')).not.toContain('<script')
  })

  it('escapes ampersands', () => {
    expect(renderContent('foo & bar')).toContain('foo &amp; bar')
  })

  it('escapes > for XSS defense-in-depth', () => {
    // Bare > in text should be escaped to &gt; even though it's harmless without <
    expect(renderContent('a > b')).toContain('&gt;')
    expect(renderContent('a > b')).not.toMatch(/[^&]>/)
  })

  // Bold and italic
  it('renders bold text', () => {
    expect(renderContent('this is **bold** text')).toContain('<strong>bold</strong>')
  })

  it('renders multiple bold segments', () => {
    const result = renderContent('**a** and **b**')
    expect(result).toContain('<strong>a</strong>')
    expect(result).toContain('<strong>b</strong>')
  })

  it('renders italic text with asterisks', () => {
    expect(renderContent('this is *italic* text')).toContain('<em>italic</em>')
  })

  it('does not render underscores as italics', () => {
    // Underscores in function names, file names, etc. should not be italic
    const result = renderContent('_foo_bar_baz_')
    expect(result).not.toContain('<em>')
    expect(result).toContain('_foo_bar_baz_')
  })

  it('preserves underscores in code context', () => {
    const result = renderContent('the function_name_with_underscores works')
    expect(result).not.toContain('<em>')
    expect(result).toContain('function_name_with_underscores')
  })

  it('renders strikethrough', () => {
    expect(renderContent('this is ~~removed~~ text')).toContain('<s>removed</s>')
  })

  // Code
  it('renders inline code', () => {
    expect(renderContent('use `npm install` here')).toContain('<code>npm install</code>')
  })

  it('renders fenced code blocks', () => {
    const result = renderContent('```js\nconst x = 1\n```')
    expect(result).toContain('<code')
    expect(result).toContain('const x = 1')
  })

  // Links
  it('renders markdown links', () => {
    const result = renderContent('see [docs](https://example.com)')
    expect(result).toContain('href="https://example.com"')
    expect(result).toContain('>docs</a>')
  })

  it('adds target="_blank" and rel="noopener" to links', () => {
    const result = renderContent('see [docs](https://example.com)')
    expect(result).toContain('target="_blank"')
    expect(result).toContain('rel="noopener"')
  })

  it('renders bare URLs as links', () => {
    const result = renderContent('visit https://example.com now')
    expect(result).toContain('href="https://example.com"')
    expect(result).toContain('target="_blank"')
  })

  it('renders bare URL at start of text', () => {
    const result = renderContent('https://example.com is the link')
    expect(result).toContain('href="https://example.com"')
  })

  it('renders bare URL after parenthesis', () => {
    const result = renderContent('(https://example.com)')
    expect(result).toContain('href="https://example.com"')
  })

  it('does not double-link markdown links as bare URLs', () => {
    const result = renderContent('[text](https://example.com)')
    // Should have exactly one link
    const linkCount = (result.match(/<a /g) || []).length
    expect(linkCount).toBe(1)
    expect(result).toContain('>text</a>')
  })

  it('does not convert URLs inside inline code', () => {
    const result = renderContent('run `curl https://example.com`')
    // URL inside code should not be wrapped in <a>
    expect(result).toContain('<code>')
    // The code element should contain the raw URL text, not a nested link
    expect(result).not.toMatch(/<code>.*<a .*<\/code>/)
  })

  // Lists
  it('renders unordered lists', () => {
    const result = renderContent('- item1\n- item2')
    expect(result).toContain('<ul>')
    expect(result).toContain('<li>item1</li>')
    expect(result).toContain('<li>item2</li>')
  })

  // Headings
  it('renders headings', () => {
    expect(renderContent('# Title')).toContain('<h1>Title</h1>')
  })

  // Blockquotes
  it('renders blockquotes', () => {
    expect(renderContent('> quoted text')).toContain('<blockquote>')
  })

  // Combined
  it('handles combined bold and links', () => {
    const result = renderContent('**bold** and [link](https://example.com)')
    expect(result).toContain('<strong>bold</strong>')
    expect(result).toContain('href="https://example.com"')
  })

  it('returns plain text unchanged (no special chars)', () => {
    expect(renderContent('hello world')).toBe('hello world')
  })
})

describe('hasMermaid', () => {
  it('returns true for text with mermaid code block', () => {
    expect(hasMermaid('```mermaid\ngraph TD\n```')).toBe(true)
  })

  it('returns false for text without mermaid', () => {
    expect(hasMermaid('just plain text')).toBe(false)
  })

  it('returns false for non-mermaid code block', () => {
    expect(hasMermaid('```javascript\nconst x = 1\n```')).toBe(false)
  })

  it('returns false for mermaid without newline after fence', () => {
    // Requires newline after ```mermaid
    expect(hasMermaid('```mermaid```')).toBe(false)
  })

  it('returns true with extra whitespace after mermaid', () => {
    expect(hasMermaid('```mermaid  \ngraph TD\n```')).toBe(true)
  })
})

describe('parseSegments', () => {
  it('returns single text segment for plain text', () => {
    expect(parseSegments('hello world')).toEqual([
      { type: 'text', content: 'hello world' },
    ])
  })

  it('extracts a single mermaid block', () => {
    const input = '```mermaid\ngraph TD\n  A --> B\n```'
    expect(parseSegments(input)).toEqual([
      { type: 'mermaid', content: 'graph TD\n  A --> B' },
    ])
  })

  it('handles text before and after mermaid block', () => {
    const input = 'before\n```mermaid\ngraph TD\n```\nafter'
    expect(parseSegments(input)).toEqual([
      { type: 'text', content: 'before\n' },
      { type: 'mermaid', content: 'graph TD' },
      { type: 'text', content: '\nafter' },
    ])
  })

  it('handles multiple mermaid blocks', () => {
    const input = 'text1\n```mermaid\nA\n```\ntext2\n```mermaid\nB\n```\ntext3'
    expect(parseSegments(input)).toEqual([
      { type: 'text', content: 'text1\n' },
      { type: 'mermaid', content: 'A' },
      { type: 'text', content: '\ntext2\n' },
      { type: 'mermaid', content: 'B' },
      { type: 'text', content: '\ntext3' },
    ])
  })

  it('trims whitespace from mermaid content', () => {
    const input = '```mermaid\n  graph TD  \n  A --> B  \n```'
    const segments = parseSegments(input)
    expect(segments[0].content).toBe('graph TD  \n  A --> B')
  })

  it('handles mermaid block at end of text', () => {
    const input = 'prefix\n```mermaid\ngraph TD\n```'
    expect(parseSegments(input)).toEqual([
      { type: 'text', content: 'prefix\n' },
      { type: 'mermaid', content: 'graph TD' },
    ])
  })

  it('handles empty input', () => {
    expect(parseSegments('')).toEqual([])
  })

  it('returns only mermaid segment for pure mermaid input', () => {
    expect(parseSegments('```mermaid\nsequenceDiagram\n```')).toEqual([
      { type: 'mermaid', content: 'sequenceDiagram' },
    ])
  })
})
