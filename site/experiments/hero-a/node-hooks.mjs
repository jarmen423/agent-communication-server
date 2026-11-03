// Lets plain `node` run the hero's TypeScript for tests: resolves the site's
// `@/` alias and extensionless relative imports to `.ts` files, and transpiles
// them with the site's own `typescript` devDependency (this Node build has no
// built-in type stripping). Register with module.register(); see scene.test.mjs.
import { existsSync, readFileSync } from "node:fs";
import ts from "typescript";
import { fileURLToPath, pathToFileURL } from "node:url";

const SITE = new URL("../../", import.meta.url);

export async function resolve(specifier, context, next) {
  let target = null;
  if (specifier.startsWith("@/")) target = new URL(specifier.slice(2), SITE);
  else if ((specifier.startsWith("./") || specifier.startsWith("../")) && context.parentURL?.endsWith(".ts")) target = new URL(specifier, context.parentURL);
  if (target && !/\.[cm]?[jt]s$/.test(target.pathname)) {
    for (const ext of [".ts", ".tsx"]) {
      const file = fileURLToPath(target) + ext;
      if (existsSync(file)) return next(pathToFileURL(file).href, context);
    }
  }
  return next(target ? target.href : specifier, context);
}

export async function load(url, context, next) {
  if (url.startsWith("file:") && url.endsWith(".ts")) {
    const source = ts.transpileModule(readFileSync(fileURLToPath(url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022, verbatimModuleSyntax: false },
    }).outputText;
    return { format: "module", source, shortCircuit: true };
  }
  return next(url, context);
}
