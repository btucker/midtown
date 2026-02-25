import { describe, it, expect } from 'vitest'
import { getSenderColor, AVENUE_COLORS } from './messageUtils.js'

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
