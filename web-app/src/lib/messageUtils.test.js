import { describe, it, expect, vi } from 'vitest'
import { getSenderColor, AVENUE_COLORS, dateChanged, getPermalinkUrl } from './messageUtils.js'

describe('getSenderColor', () => {
  it('returns gold for sender matching channel name (channel lead rule)', () => {
    expect(getSenderColor('web', undefined, 'web')).toBe(AVENUE_COLORS.lead)
  })

  it('returns gold for midtown sender in midtown channel via channel lead rule', () => {
    expect(getSenderColor('midtown', undefined, 'midtown')).toBe(AVENUE_COLORS.lead)
  })

  it('returns gold for midtown sender even without channelName (AVENUE_COLORS fallback)', () => {
    expect(getSenderColor('midtown', undefined)).toBe(AVENUE_COLORS.lead)
  })

  it('does not color a non-lead sender gold when channelName is provided', () => {
    expect(getSenderColor('lexington', undefined, 'web')).toBe(AVENUE_COLORS.lexington)
  })

  it('is case-insensitive for channel name matching', () => {
    expect(getSenderColor('Web', undefined, 'web')).toBe(AVENUE_COLORS.lead)
    expect(getSenderColor('web', undefined, 'Web')).toBe(AVENUE_COLORS.lead)
  })

  it('returns fallback gray for unknown sender with no channel match', () => {
    expect(getSenderColor('unknown', undefined, 'web')).toBe('#d0d0d0')
  })

  it('respects overrides before AVENUE_COLORS lookup', () => {
    const overrides = { lexington: '#ff0000' }
    expect(getSenderColor('lexington', overrides, 'web')).toBe('#ff0000')
  })

  it('channel lead rule takes priority over overrides', () => {
    // Even if an override exists for 'web', sender=channel → gold wins
    const overrides = { web: '#ff0000' }
    expect(getSenderColor('web', overrides, 'web')).toBe(AVENUE_COLORS.lead)
  })
})

describe('dateChanged', () => {
  function msg(timestamp) {
    return { timestamp, from: 'test', content: 'hello' }
  }

  it('returns null for the first message (index 0)', () => {
    const msgs = [msg('2026-03-02T10:00:00Z')]
    expect(dateChanged(msgs, 0)).toBe(null)
  })

  it('returns null when messages are on the same day and within 8 hours', () => {
    const msgs = [
      msg('2026-03-02T10:00:00Z'),
      msg('2026-03-02T14:00:00Z'),
    ]
    expect(dateChanged(msgs, 1)).toBe(null)
  })

  it('returns a date label when the calendar date changes', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-03-02T20:00:00Z'))
    const msgs = [
      msg('2026-03-01T12:00:00Z'),
      msg('2026-03-02T12:00:00Z'),
    ]
    expect(dateChanged(msgs, 1)).toBe('Today')
    vi.useRealTimers()
  })

  it('returns "Yesterday" for the previous day', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-03-03T12:00:00Z'))
    const msgs = [
      msg('2026-03-01T23:00:00Z'),
      msg('2026-03-02T10:00:00Z'),
    ]
    expect(dateChanged(msgs, 1)).toBe('Yesterday')
    vi.useRealTimers()
  })

  it('returns full date for older messages', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-03-10T12:00:00Z'))
    const msgs = [
      msg('2026-02-27T23:00:00Z'),
      msg('2026-02-28T10:00:00Z'),
    ]
    const result = dateChanged(msgs, 1)
    expect(result).toContain('February')
    expect(result).toContain('28')
    expect(result).toContain('2026')
    vi.useRealTimers()
  })

  it('returns a date label for 8+ hour gap on the same day', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-03-02T22:00:00Z'))
    const msgs = [
      msg('2026-03-02T02:00:00Z'),
      msg('2026-03-02T10:00:01Z'),  // 8 hours + 1 second gap
    ]
    expect(dateChanged(msgs, 1)).toBe('Today')
    vi.useRealTimers()
  })

  it('returns null for gap just under 8 hours on the same day', () => {
    const msgs = [
      msg('2026-03-02T14:00:00Z'),
      msg('2026-03-02T21:59:59Z'),  // 7h 59m 59s gap
    ]
    expect(dateChanged(msgs, 1)).toBe(null)
  })

  it('returns null for invalid timestamps', () => {
    const msgs = [
      msg('invalid'),
      msg('2026-03-02T10:00:00Z'),
    ]
    expect(dateChanged(msgs, 1)).toBe(null)
  })
})

describe('getPermalinkUrl', () => {
  it('generates thread-level URL for channel messages (no threadParentId)', () => {
    expect(getPermalinkUrl('myproject', 'web', 'msg-123')).toBe(
      '/myproject?channel=web&thread=msg-123'
    )
  })

  it('generates message-level URL for thread replies (with threadParentId)', () => {
    expect(getPermalinkUrl('myproject', 'web', 'reply-456', 'parent-123')).toBe(
      '/myproject?channel=web&thread=parent-123&msg=reply-456'
    )
  })

  it('encodes special characters in URL components', () => {
    const url = getPermalinkUrl('my project', 'web channel', 'msg&id', 'parent=id')
    expect(url).toBe('/my%20project?channel=web%20channel&thread=parent%3Did&msg=msg%26id')
  })

  it('returns empty string when projectName is missing', () => {
    expect(getPermalinkUrl(null, 'web', 'msg-123')).toBe('')
    expect(getPermalinkUrl('', 'web', 'msg-123')).toBe('')
  })

  it('returns empty string when channelName is missing', () => {
    expect(getPermalinkUrl('myproject', null, 'msg-123')).toBe('')
    expect(getPermalinkUrl('myproject', '', 'msg-123')).toBe('')
  })

  it('returns empty string when msgId is missing', () => {
    expect(getPermalinkUrl('myproject', 'web', null)).toBe('')
    expect(getPermalinkUrl('myproject', 'web', '')).toBe('')
  })
})
