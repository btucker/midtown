#!/usr/bin/env node

const fs = require('node:fs')
const path = require('node:path')

const root = path.resolve(__dirname, '..')
const nodeModulesRoot = path.join(root, 'node_modules')
const fsExtraSuffix = `${path.sep}fs-extra${path.sep}lib${path.sep}fs${path.sep}index.js`
const workboxErrorsSuffix = `${path.sep}workbox-build${path.sep}build${path.sep}lib${path.sep}errors.js`
const getIntrinsicPackages = new Set(['set-function-length', 'call-bound', 'call-bind', 'es-abstract'])

function collectFilesBySuffix(baseDir, suffix) {
  const matches = []
  if (!fs.existsSync(baseDir)) {
    return matches
  }

  const stack = [baseDir]
  while (stack.length > 0) {
    const current = stack.pop()
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) {
        stack.push(full)
        continue
      }

      if (full.endsWith(suffix)) {
        matches.push(full)
      }
    }
  }

  return matches
}

function collectPackageDirs(baseDir, packageNames) {
  const matches = []
  if (!fs.existsSync(baseDir)) {
    return matches
  }

  const stack = [baseDir]
  while (stack.length > 0) {
    const current = stack.pop()
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (!entry.isDirectory()) {
        continue
      }

      const full = path.join(current, entry.name)
      if (packageNames.has(entry.name)) {
        matches.push(full)
      }
      stack.push(full)
    }
  }

  return matches
}

function walkGetIntrinsicFiles(target) {
  const stack = [target]
  while (stack.length > 0) {
    const current = stack.pop()
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name)
      if (entry.isDirectory()) {
        stack.push(full)
        continue
      }

      if (!entry.isFile() || !entry.name.endsWith('.js')) {
        continue
      }

      const contents = fs.readFileSync(full, 'utf8')
      if (contents.includes("require('get-intrinsic')")) {
        patchGetIntrinsicImport(full)
      }
    }
  }
}

function patchGetIntrinsicImport(filePath) {
  const original = fs.readFileSync(filePath, 'utf8')
  const lines = original.split(/\r?\n/)
  const updatedLines = []

  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i]
    const match = line.match(/^(\s*)(?:var|const)\s+(\w+)\s*=\s*require\('get-intrinsic'\);$/)
    if (!match) {
      updatedLines.push(line)
      continue
    }

    const indent = match[1]
    const binding = match[2]
    const fallbackLine = `${indent}${binding} = ${binding}.default || ${binding};`
    updatedLines.push(line)

    let nextIndex = i + 1
    while (nextIndex < lines.length && lines[nextIndex].trim() === `${binding} = ${binding}.default || ${binding};`) {
      nextIndex += 1
    }

    if (nextIndex === i + 1) {
      updatedLines.push(fallbackLine)
      changed = true
    }

    i = nextIndex - 1
  }

  const updated = updatedLines.join('\n')
  if (updated !== original) {
    fs.writeFileSync(filePath, updated)
  }
}

function patchWorkboxErrors() {
  const errorsPath = collectFilesBySuffix(nodeModulesRoot, workboxErrorsSuffix)
  for (const filePath of errorsPath) {
    patchWorkboxErrorsFile(filePath)
  }
}

function patchWorkboxErrorsFile(errorsPath) {
  const original = fs.readFileSync(errorsPath, 'utf8')
  const importMarker = 'const common_tags_1 = require("common-tags");'
  const compatMarker = 'const common_tags_1_default = common_tags_1.default || common_tags_1;'
  const lines = original.split(/\r?\n/)
  const dedupedLines = []

  for (const line of lines) {
    const trimmed = line.trim()
    if (
      trimmed === compatMarker &&
      dedupedLines.length > 0 &&
      dedupedLines[dedupedLines.length - 1].trim() === compatMarker
    ) {
      continue
    }
    dedupedLines.push(line)
  }

  const importIndex = dedupedLines.findIndex((line) => line.includes(importMarker))
  if (importIndex === -1) {
    return
  }

  const compatCandidate = dedupedLines[importIndex + 1]?.trim() === compatMarker
  if (!compatCandidate) {
    dedupedLines.splice(importIndex + 1, 0, compatMarker)
  }

  let updated = dedupedLines.join('\n')
  if (updated.includes('(0, common_tags_1.oneLine)')) {
    const normalized = updated.replace(
      /\(0,\s*common_tags_1\.oneLine\)/g,
      '(0, common_tags_1_default.oneLine)',
    )
    if (normalized !== updated) {
      updated = normalized
    }
  }

  if (updated !== original) {
    fs.writeFileSync(errorsPath, updated)
  }
}

function patchFsExtra() {
  const targets = collectFilesBySuffix(nodeModulesRoot, fsExtraSuffix)
  for (const filePath of targets) {
    patchFsExtraFile(filePath)
  }
}

function patchFsExtraFile(filePath) {
  const marker = 'if (typeof fs.realpath.native === \'function\') {'
  const fixed = 'if (fs.realpath && typeof fs.realpath.native === \'function\') {'

  const original = fs.readFileSync(filePath, 'utf8')
  if (original.includes(marker)) {
    const updated = original.replace(marker, fixed)
    if (updated !== original) {
      fs.writeFileSync(filePath, updated)
    }
  }
}

try {
  if (fs.existsSync(nodeModulesRoot)) {
    patchFsExtra()
    patchWorkboxErrors()
    for (const packageDir of collectPackageDirs(nodeModulesRoot, getIntrinsicPackages)) {
      walkGetIntrinsicFiles(packageDir)
    }
  }
} catch (err) {
  // Keep installs non-blocking in environments where node_modules may be partially
  // installed; this mirrors the defensive behavior expected by midtown startup.
  console.error('postinstall patch failed:', err.message)
}
