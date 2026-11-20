// Publishes a release to the public Vista-app repository, where phones look
// for updates (src-tauri/src/updater.rs):
//   node scripts/publish.mjs 0.5.0 notes.md
// Run it after scripts/release.mjs built Vista-<version>.apk and the code is
// committed. It copies the committed code (git archive HEAD) into a clone of
// Vista-app, without CLAUDE.md, PLAN.md, .claude/ and docs/README.public.md
// (which becomes its README.md), commits it as "Vista <version>" under
// GitHub's noreply address, pushes, and creates release v<version> with the
// APK and the notes (shown in the app: **bold** and "- " bullets work).
// The code is public because youtubedl-android is GPL-3.0. Needs `gh`
// signed in; creates the repository the first time.
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const REPO = "octavianbanarizatic-hub/Vista-app";
const AUTHOR = ["-c", "user.name=nabo", "-c", "user.email=255198355+octavianbanarizatic-hub@users.noreply.github.com"];
/** Left out of the public copy. */
const PRIVATE = ["CLAUDE.md", "PLAN.md", ".claude", "docs/README.public.md"];

const root = join(import.meta.dirname, "..");
const [version, notes] = process.argv.slice(2);
const fail = (msg) => {
  console.error(msg);
  process.exit(1);
};
const run = (cmd, args, cwd = root, quiet = false) =>
  spawnSync(cmd, args, { cwd, encoding: "utf8", stdio: quiet ? "pipe" : "inherit" });
const must = (cmd, args, cwd = root) => {
  const r = run(cmd, args, cwd);
  if (r.status !== 0) fail(`${cmd} ${args.join(" ")} failed.`);
};

if (!/^\d+\.\d+\.\d+$/.test(version ?? "") || !notes) fail("Usage: node scripts/publish.mjs <version> <notes.md>");
if (!existsSync(notes)) fail(`No notes file at ${notes}.`);
const conf = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8")).version;
if (conf !== version) fail(`The app's version is ${conf}, not ${version}: run scripts/release.mjs ${version} first.`);
const apk = join(root, `Vista-${version}.apk`);
if (!existsSync(apk)) fail(`No ${apk}: run scripts/release.mjs ${version} first.`);
// The public code has to be what the APK was built from.
const dirty = run("git", ["status", "--porcelain", "--untracked-files=no"], root, true).stdout.trim();
if (dirty) fail("Commit the changes first; the public copy is made from the last commit.");

if (run("gh", ["repo", "view", REPO], root, true).status !== 0) {
  must("gh", ["repo", "create", REPO, "--public", "--description", "A light, ad-free YouTube app for Android: no account, no tracking."]);
}

const work = mkdtempSync(join(tmpdir(), "vista-publish-"));
const clone = join(work, "Vista-app");
must("gh", ["repo", "clone", REPO, clone]);
// Everything but its history goes; the commit brings the new copy.
for (const name of readdirSync(clone)) if (name !== ".git") rmSync(join(clone, name), { recursive: true, force: true });
must("git", ["archive", "--format=tar", "-o", join(work, "code.tar"), "HEAD"]);
// Relative names: some tars read "C:" in a path as a remote host.
must("tar", ["-xf", "code.tar", "-C", "Vista-app"], work);
for (const p of PRIVATE) rmSync(join(clone, p), { recursive: true, force: true });
copyFileSync(join(root, "docs", "README.public.md"), join(clone, "README.md"));

// A new, empty repository has no branch yet.
must("git", ["checkout", "-q", "-B", "main"], clone);
must("git", ["add", "-A"], clone);
if (run("git", ["diff", "--cached", "--quiet"], clone, true).status !== 0) {
  must("git", [...AUTHOR, "commit", "-q", "-m", `Vista ${version}`], clone);
  must("git", ["push", "-q", "origin", "main"], clone);
}
must("gh", ["release", "create", `v${version}`, apk, "-R", REPO, "--title", `Vista ${version}`, "--notes-file", notes]);
rmSync(work, { recursive: true, force: true });
console.log(`\nPublished https://github.com/${REPO}/releases/tag/v${version}`);
