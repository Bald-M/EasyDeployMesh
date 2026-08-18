import { copyFile, mkdir, readFile, readdir } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import process from 'node:process'
import { desktopTarget } from './desktop-targets.mjs'

const projectRoot = resolve(import.meta.dirname, '..')
const outputDirectory = join(projectRoot, 'release')
const tauriConfig = JSON.parse(
  await readFile(join(projectRoot, 'apps', 'desktop', 'src-tauri', 'tauri.conf.json'), 'utf8'),
)

const targetName = process.argv[2]
const target = desktopTarget(targetName)
const installer = {
  directory: join(projectRoot, 'target', target.rustTarget, 'release', 'bundle', target.bundles),
  extension: target.extension,
}

const versionMarker = `_${tauriConfig.version}_`
const files = (await readdir(installer.directory)).filter(
  (file) => file.includes(versionMarker) && file.endsWith(installer.extension),
)

if (files.length === 0) {
  console.error(`No ${installer.extension} installer found in ${installer.directory}`)
  process.exit(1)
}

await mkdir(outputDirectory, { recursive: true })

for (const file of files) {
  const source = join(installer.directory, file)
  const suffix = file.endsWith('-setup.exe') ? '-setup.exe' : target.extension
  const destination = join(
    outputDirectory,
    `${tauriConfig.productName}-${target.artifactLabel}-${tauriConfig.version}${suffix}`,
  )
  await copyFile(source, destination)
  console.log(`Installer copied to ${destination}`)
}
