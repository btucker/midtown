export const AVENUE_COLORS = {
  lexington: '#5fafaf',
  park: '#5faf5f',
  madison: '#ff5f5f',
  broadway: '#af5faf',
  amsterdam: '#5f87af',
  columbus: '#af5f5f',
  riverside: '#87d7d7',
  york: '#87d787',
  pleasant: '#d7afd7',
  vernon: '#87afd7',
  bleecker: '#d7875f',
  houston: '#ff87d7',
  canal: '#87d7ff',
  spring: '#afff87',
  prince: '#d7afff',
  mercer: '#ffaf87',
  lead: '#E3BD3F',
  github: '#585858',
  system: '#585858',
  daemon: '#585858',
  midtown: '#E3BD3F',
}

export function getAvenueColor(name, fallback = '#d0d0d0') {
  return AVENUE_COLORS[name?.toLowerCase()] || fallback
}
