import { describe, it, expect } from 'vitest'
import { parseSegments, hasMermaid, renderContent } from './markdown.js'

describe('renderContent', () => {
  it('escapes HTML angle brackets', () => {
    expect(renderContent('<script>alert("xss")</script>')).toBe(
      '&lt;script&gt;alert("xss")&lt;/script&gt;'
    )
  })

  it('escapes ampersands', () => {
    expect(renderContent('foo & bar')).toBe('foo &amp; bar')
  })

  it('renders bold text', () => {
    expect(renderContent('this is **bold** text')).toBe(
      'this is <strong>bold</strong> text'
    )
  })

  it('renders multiple bold segments', () => {
    expect(renderContent('**a** and **b**')).toBe(
      '<strong>a</strong> and <strong>b</strong>'
    )
  })

  it('renders markdown links', () => {
    expect(renderContent('see [docs](https://example.com)')).toBe(
      'see <a href="https://example.com" target="_blank" rel="noopener">docs</a>'
    )
  })

  it('renders bare URLs', () => {
    expect(renderContent('visit https://example.com now')).toBe(
      'visit <a href="https://example.com" target="_blank" rel="noopener">https://example.com</a> now'
    )
  })

  it('renders bare URL at start of text', () => {
    expect(renderContent('https://example.com is the link')).toBe(
      '<a href="https://example.com" target="_blank" rel="noopener">https://example.com</a> is the link'
    )
  })

  it('renders bare URL after parenthesis', () => {
    expect(renderContent('(https://example.com)')).toBe(
      '(<a href="https://example.com" target="_blank" rel="noopener">https://example.com</a>)'
    )
  })

  it('does not double-link markdown links as bare URLs', () => {
    const result = renderContent('[text](https://example.com)')
    // Should produce exactly one <a> tag, not nested
    expect(result).toBe('<a href="https://example.com" target="_blank" rel="noopener">text</a>')
  })

  it('handles combined bold and links', () => {
    const result = renderContent('**bold** and [link](https://example.com)')
    expect(result).toContain('<strong>bold</strong>')
    expect(result).toContain('<a href="https://example.com"')
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
