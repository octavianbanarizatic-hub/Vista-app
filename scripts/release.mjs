// Builds the signed release APK for the phone under a new version number:
//   node scripts/release.mjs 0.4.0
// Sets the version in tauri.conf.json (which the app and Android read),
// package.json and Cargo.toml, builds for 64-bit phones, and copies the APK
// to Vista-<version>.apk in the project folder, removing older copies.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const root = join(import.meta.dirname, "..");
const version = process.argv[2];
const fail = (msg) => {
  console.error(msg);
  process.exit(1);
};

if (!/^\d+\.\d+\.\d+$/.test(version ?? "")) fail("Usage: node scripts/release.mjs <version>, like 0.4.0");

const conf = join(root, "src-tauri", "tauri.conf.json");
const current = JSON.parse(readFileSync(conf, "utf8")).version;
/** Above zero when version `a` is higher than `b`. */
function compare(a, b) {
  const [x, y] = [a, b].map((v) => v.split(".").map(Number));
  return x.map((n, i) => n - y[i]).find((d) => d !== 0) ?? 0;
}
// Android only installs an update with a higher version over the installed app.
if (compare(version, current) <= 0) fail(`The new version must be higher than ${current}.`);

// Without the key, the APK comes out unsigned and the phone won't install it.
let signing = false;
for (let dir = root; ; dir = dirname(dir)) {
  if (existsSync(join(dir, ".signing", "keystore.properties"))) {
    signing = true;
    break;
  }
  if (dirname(dir) === dir) break;
}
if (!signing) fail("The signing key (.signing folder) is missing, so the phone couldn't install this update.");

/** Replaces the first match of `pattern` in a file, keeping its line endings. */
function bump(file, pattern) {
  const path = join(root, file);
  const text = readFileSync(path, "utf8");
  if (!pattern.test(text)) fail(`No version found in ${file}.`);
  writeFileSync(path, text.replace(pattern, (m) => m.replace(current, version)));
}
bump("src-tauri/tauri.conf.json", /"version": "[^"]+"/);
bump("package.json", /"version": "[^"]+"/);
bump("src-tauri/Cargo.toml", /^version = "[^"]+"/m);
console.log(`Version ${current} → ${version}. Building the phone APK...`);

const build = spawnSync("node", [join(root, "scripts", "android.mjs"), "build", "--apk", "--target", "aarch64"], {
  cwd: root,
  stdio: "inherit",
});
if (build.status !== 0) fail("The build failed; see the output above. The version numbers stay changed.");

const apk = join(root, "src-tauri", "gen", "android", "app", "build", "outputs", "apk", "arm64", "release", "app-arm64-release.apk");
if (!existsSync(apk)) fail(`The build finished but there's no APK at ${apk}.`);
for (const old of readdirSync(root).filter((f) => /^Vista-.*\.apk$/.test(f))) rmSync(join(root, old));
const out = join(root, `Vista-${version}.apk`);
copyFileSync(apk, out);
console.log(`\nDone: ${out}`);
console.log(`Install it on the phone (USB debugging on), in PowerShell:`);
console.log(`& "$env:LOCALAPPDATA\\Android\\Sdk\\platform-tools\\adb.exe" install -r "${out}"`);
