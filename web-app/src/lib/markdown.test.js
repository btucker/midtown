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
    const result = renderContent('a > b')
    expect(result).toContain('&gt;')
    // The only > characters in the output should be from HTML tags, not raw text
    expect(result).not.toContain('a > b')
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
    // marked (GFM) uses <del> for strikethrough, which is the HTML5 standard element
    expect(renderContent('this is ~~removed~~ text')).toContain('<del>removed</del>')
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

  it('renders plain text wrapped in a paragraph', () => {
    // marked wraps block-level content in <p> tags
    const result = renderContent('hello world')
    expect(result).toContain('hello world')
    expect(result).toContain('<p>')
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

describe('renderContent - special links', () => {
  // Channel links
  it('converts #channel references to clickable links', () => {
    const result = renderContent('See #midtown for updates')
    expect(result).toContain('class="channel-link"')
    expect(result).toContain('data-channel="midtown"')
    expect(result).toContain('#midtown</a>')
  })

  it('handles multiple #channel references', () => {
    const result = renderContent('Check #midtown and #brooklyn')
    expect(result).toMatch(/class="channel-link".*class="channel-link"/)
    expect(result).toContain('data-channel="midtown"')
    expect(result).toContain('data-channel="brooklyn"')
  })

  it('does not create nested <a> tags when #channel is in markdown link', () => {
    const result = renderContent('[See #midtown](https://example.com)')
    // Should not have nested <a> tags
    expect(result).not.toMatch(/<a[^>]*><a/)
    expect(result).not.toMatch(/<\/a><\/a>/)
  })

  it('does not create nested <a> tags when #channel is in inline code', () => {
    const result = renderContent('Use `#midtown` channel')
    // Should not have <a> inside <code>
    expect(result).not.toMatch(/<code[^>]*>.*<a/)
  })

  // Task links
  it('converts !N task references to clickable links', () => {
    const result = renderContent('Working on !42')
    expect(result).toContain('class="task-link"')
    expect(result).toContain('data-task="42"')
    expect(result).toContain('!42</a>')
  })

  it('handles multiple !N task references', () => {
    const result = renderContent('Tasks !1, !2, and !3')
    expect(result).toMatch(/class="task-link".*class="task-link".*class="task-link"/)
    expect(result).toContain('data-task="1"')
    expect(result).toContain('data-task="2"')
    expect(result).toContain('data-task="3"')
  })

  it('does not create nested <a> tags when !task is in markdown link', () => {
    const result = renderContent('[Task !42](https://example.com)')
    // Should not have nested <a> tags
    expect(result).not.toMatch(/<a[^>]*><a/)
    expect(result).not.toMatch(/<\/a><\/a>/)
  })

  // PR links
  it('converts PR #N references to clickable links', () => {
    const result = renderContent('See PR #123')
    expect(result).toContain('class="pr-link"')
    expect(result).toContain('data-pr="123"')
    expect(result).toContain('PR #123</a>')
  })

  it('converts bare #N references to PR links', () => {
    const result = renderContent('Merged #456')
    expect(result).toContain('class="pr-link"')
    expect(result).toContain('data-pr="456"')
    expect(result).toContain('#456</a>')
  })

  it('does not create nested <a> tags when PR reference is in markdown link', () => {
    const result = renderContent('[PR #123](https://github.com/org/repo/pull/123)')
    // Should not have nested <a> tags
    expect(result).not.toMatch(/<a[^>]*><a/)
    expect(result).not.toMatch(/<\/a><\/a>/)
  })

  // Combined scenarios
  it('handles mixed special links in same message', () => {
    const result = renderContent('Working on !42 in #midtown, see PR #123')
    expect(result).toContain('class="task-link"')
    expect(result).toContain('class="channel-link"')
    expect(result).toContain('class="pr-link"')
  })

  // Issue #1: Code/pre tag exclusion - #references inside inline code should NOT be linked
  it('does not convert #N inside inline code to PR link', () => {
    const result = renderContent('run `git checkout #123`')
    // #123 inside backticks should NOT be converted to a clickable link
    expect(result).not.toMatch(/<code[^>]*>.*<a /)
    expect(result).toContain('<code>')
  })

  it('does not convert #channel inside inline code to channel link', () => {
    const result = renderContent('use `#midtown` config')
    // Already tested above but verify no channel-link inside code
    expect(result).not.toMatch(/<code[^>]*>.*class="channel-link"/)
  })

  it('does not convert !task inside inline code to task link', () => {
    const result = renderContent('see `!42` for details')
    expect(result).not.toMatch(/<code[^>]*>.*class="task-link"/)
  })

  // Issue #2: PR link regex collision - "PR #456" should produce exactly one link
  it('does not create nested anchors for PR #N references', () => {
    const result = renderContent('Check PR #456 for details')
    // Should have exactly one <a> wrapping "PR #456", not nested anchors
    const anchorCount = (result.match(/<a /g) || []).length
    expect(anchorCount).toBe(1)
    expect(result).toContain('class="pr-link"')
    expect(result).toContain('PR #456</a>')
  })

  // Issue #4: pr-link anchors should NOT have target="_blank"
  it('does not add target="_blank" to pr-link anchors', () => {
    const result = renderContent('see #42')
    expect(result).toContain('class="pr-link"')
    // pr-link should NOT have target="_blank" since it's handled by event delegation
    expect(result).not.toMatch(/target="_blank"[^>]*class="pr-link"/)
    expect(result).not.toMatch(/class="pr-link"[^>]*target="_blank"/)
  })

  it('does not add target="_blank" to channel-link anchors', () => {
    const result = renderContent('see #midtown')
    expect(result).toContain('class="channel-link"')
    expect(result).not.toMatch(/target="_blank"[^>]*class="channel-link"/)
    expect(result).not.toMatch(/class="channel-link"[^>]*target="_blank"/)
  })

  it('does not add target="_blank" to task-link anchors', () => {
    const result = renderContent('see !42')
    expect(result).toContain('class="task-link"')
    expect(result).not.toMatch(/target="_blank"[^>]*class="task-link"/)
    expect(result).not.toMatch(/class="task-link"[^>]*target="_blank"/)
  })
})

describe('renderContent - tables', () => {
  it('renders a simple GFM table', () => {
    const input = '| A | B |\n|---|---|\n| 1 | 2 |'
    const result = renderContent(input)
    expect(result).toContain('<table>')
    expect(result).toContain('<th>')
    expect(result).toContain('<td>')
    expect(result).toContain('</table>')
  })

  it('renders table header cells', () => {
    const input = '| Column A | Column B |\n|----------|----------|\n| Row 1    | Data     |'
    const result = renderContent(input)
    expect(result).toContain('<th>Column A</th>')
    expect(result).toContain('<th>Column B</th>')
  })

  it('renders table data cells', () => {
    const input = '| Column A | Column B |\n|----------|----------|\n| Row 1    | Data     |'
    const result = renderContent(input)
    expect(result).toContain('<td>Row 1</td>')
    expect(result).toContain('<td>Data</td>')
  })

  it('renders multiple data rows', () => {
    const input = '| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |'
    const result = renderContent(input)
    expect(result).toContain('<td>1</td>')
    expect(result).toContain('<td>2</td>')
    expect(result).toContain('<td>3</td>')
    expect(result).toContain('<td>4</td>')
  })

  it('renders text before and after the table', () => {
    const input = 'Before\n| A | B |\n|---|---|\n| 1 | 2 |\nAfter'
    const result = renderContent(input)
    expect(result).toContain('Before')
    expect(result).toContain('<table>')
    expect(result).toContain('After')
  })

  it('supports inline markdown in table cells', () => {
    const input = '| A | B |\n|---|---|\n| **bold** | `code` |'
    const result = renderContent(input)
    expect(result).toContain('<strong>bold</strong>')
    expect(result).toContain('<code>code</code>')
  })

  it('does not treat plain text with pipes as a table', () => {
    const result = renderContent('this | that')
    expect(result).not.toContain('<table>')
  })

  it('does not treat a single-row pipe line as a table (no separator row)', () => {
    const result = renderContent('| A | B |')
    expect(result).not.toContain('<table>')
  })
})

describe('renderContent - image attachments', () => {
  const API = 'http://localhost:47023/api'

  it('renders [Attached: /path/file.png] as an inline image', () => {
    const result = renderContent('[Attached: /home/user/.midtown/projects/mid/uploads/20260101-file.png]', API)
    expect(result).toContain('<img')
    expect(result).toContain('src="http://localhost:47023/api/uploads/20260101-file.png"')
    expect(result).toContain('class="message-image"')
  })

  it('wraps the image in a clickable link', () => {
    const result = renderContent('[Attached: /uploads/photo.png]', API)
    expect(result).toContain('<a')
    expect(result).toContain('target="_blank"')
    expect(result).toContain('class="attachment-link"')
  })

  it('renders [Attached file:]\\nPlease read: /path/file.png as an image', () => {
    const input = '[Attached file: photo.png]\nPlease read: /home/user/.midtown/projects/mid/uploads/20260101-photo.png'
    const result = renderContent(input, API)
    expect(result).toContain('<img')
    expect(result).toContain('src="http://localhost:47023/api/uploads/20260101-photo.png"')
  })

  it('renders a non-image attachment as a file badge', () => {
    const result = renderContent('[Attached: /uploads/report.pdf]', API)
    expect(result).not.toContain('<img')
    expect(result).toContain('class="attachment-ref"')
    expect(result).toContain('report.pdf')
  })

  it('renders message text followed by attachment correctly', () => {
    const result = renderContent('Here is the screenshot\n\n[Attached: /uploads/shot.png]', API)
    expect(result).toContain('Here is the screenshot')
    expect(result).toContain('<img')
  })

  it('shows file badge when no apiBase is provided', () => {
    const result = renderContent('[Attached: /uploads/photo.png]')
    expect(result).not.toContain('<img')
    expect(result).toContain('class="attachment-ref"')
    expect(result).toContain('photo.png')
  })

  it('handles filenames with spaces in [Attached file:]\\nPlease read: format', () => {
    const input = '[Attached file: my photo.png]\nPlease read: /home/user/.midtown/projects/mid/uploads/20260101-my photo.png'
    const result = renderContent(input, API)
    expect(result).toContain('<img')
    expect(result).toContain('20260101-my%20photo.png')
  })

  it('XSS: attachment path with special chars is escaped in alt text', () => {
    const result = renderContent('[Attached: /uploads/file"name.png]', API)
    expect(result).not.toContain('"name')
    expect(result).toContain('&quot;')
  })
})
