// Runs `tauri android <args>` with the Android toolchain from
// %LOCALAPPDATA%\Android (Java 21, SDK, NDK), without changing system-wide
// settings. Examples:
//   node scripts/android.mjs build --apk --target aarch64 armv7
//   node scripts/android.mjs build --apk --debug --target x86_64
//
// Tauri links the compiled Rust library into the Android project with a
// symbolic link, which Windows only allows in Developer Mode. If that step
// fails, this copies the library instead and runs the Gradle step itself.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";

const root = join(import.meta.dirname, "..");
const android = join(process.env.LOCALAPPDATA ?? "", "Android");
const sdk = join(android, "Sdk");
const ndkDir = join(sdk, "ndk");
const ndks = existsSync(ndkDir) ? readdirSync(ndkDir).sort((a, b) => a.localeCompare(b, undefined, { numeric: true })) : [];
if (!ndks.length) {
  console.error(`No Android NDK in ${ndkDir}. See the README's Android section.`);
  process.exit(1);
}
const env = {
  ...process.env,
  JAVA_HOME: join(android, "jdk-21"),
  ANDROID_HOME: sdk,
  NDK_HOME: join(ndkDir, ndks.at(-1)),
};
env.Path = [join(env.JAVA_HOME, "bin"), join(sdk, "platform-tools"), process.env.Path ?? process.env.PATH].join(";");

// The release signing key lives in a .signing folder outside git: next to
// the repo, or above it when building from a worktree inside it.
for (let dir = root; ; dir = dirname(dir)) {
  const props = join(dir, ".signing", "keystore.properties");
  if (existsSync(props)) {
    env.VISTA_KEYSTORE_PROPERTIES = props;
    break;
  }
  if (dirname(dir) === dir) break;
}

const args = process.argv.slice(2);
const run = (cmd, cmdArgs, cwd = root) =>
  spawnSync(cmd, cmdArgs, { cwd, env, shell: true, encoding: "utf8", stdio: ["inherit", "pipe", "pipe"] });

const tauri = run("npx", ["tauri", "android", ...args]);
process.stdout.write(tauri.stdout);
process.stderr.write(tauri.stderr);
if (tauri.status === 0) process.exit(0);
if (args[0] !== "build" || !/symbolic link/i.test(tauri.stdout + tauri.stderr)) process.exit(tauri.status ?? 1);

// Same names the generated Gradle project uses.
const targets = {
  aarch64: { triple: "aarch64-linux-android", abi: "arm64-v8a", arch: "Arm64" },
  armv7: { triple: "armv7-linux-androideabi", abi: "armeabi-v7a", arch: "Arm" },
  i686: { triple: "i686-linux-android", abi: "x86", arch: "X86" },
  x86_64: { triple: "x86_64-linux-android", abi: "x86_64", arch: "X86_64" },
};
const at = args.findIndex((a) => a === "--target" || a === "-t");
const picked = at < 0 ? Object.keys(targets) : args.slice(at + 1).filter((a) => !a.startsWith("-"));
const debug = args.includes("--debug") || args.includes("-d");
const profile = debug ? "debug" : "release";
const Profile = debug ? "Debug" : "Release";
const project = join(root, "src-tauri", "gen", "android");

for (const [i, name] of picked.entries()) {
  const t = targets[name];
  if (!t) throw new Error(`unknown target ${name}`);
  // Tauri stops at the first target's link, so build the others one by one
  // (each stops at its own link, after compiling).
  if (i > 0) {
    console.log(`\nBuilding the Rust library for ${name}...`);
    const oneArgs = at < 0 ? [...args, "--target", name] : [...args.slice(0, at + 1), name, ...args.slice(at + 1 + picked.length)];
    const one = run("npx", ["tauri", "android", ...oneArgs]);
    if (one.status !== 0 && !/symbolic link/i.test(one.stdout + one.stderr)) {
      process.stdout.write(one.stdout);
      process.stderr.write(one.stderr);
      process.exit(one.status ?? 1);
    }
  }
  console.log(`Copying the Rust library for ${name} instead of linking it...`);
  const dir = join(project, "app", "src", "main", "jniLibs", t.abi);
  mkdirSync(dir, { recursive: true });
  copyFileSync(join(root, "src-tauri", "target", t.triple, profile, "libvista_lib.so"), join(dir, "libvista_lib.so"));
}

const flavor = picked.length === 1 ? targets[picked[0]].arch : "Universal";
const task = args.includes("--aab") ? `bundle${flavor}${Profile}` : `assemble${flavor}${Profile}`;
console.log(`Running Gradle ${task}...`);
// As Tauri does: only these ABIs go into the APK, also for libraries that
// ship native code for every ABI (youtubedl-android's Python).
const abiList = `-PabiList=${picked.map((n) => targets[n].abi).join(",")}`;
const gradle = run(".\\gradlew.bat", [task, abiList, "-x", `rustBuild${flavor}${Profile}`, "--console=plain"], project);
process.stdout.write(gradle.stdout);
process.stderr.write(gradle.stderr);
if (gradle.status === 0) {
  console.log(`\nDone: ${join(project, "app", "build", "outputs")}`);
}
process.exit(gradle.status ?? 1);
