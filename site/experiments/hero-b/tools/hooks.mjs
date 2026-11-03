// Module hooks so plain `node` can run the hero's own TypeScript modules (the
// test and the poster bake) without a bundler or a new dependency:
//   - extensionless relative and `@/` imports resolve to `.ts`,
//   - `.ts` is transpiled with the site's existing `typescript` package,
//   - `.wgsl` imports become prepared shaders, like the @vgpu/wgsl loader,
//   - `vgpu` maps to HERO_VGPU (`vgpu/mock` for tests, `vgpu/node` for bakes).
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const SITE = new URL("../../../", import.meta.url);
const VGPU = process.env.HERO_VGPU ?? "vgpu/mock";

export async function resolve(specifier, context, next) {
  let spec = specifier;
  if (spec === "vgpu") return next(VGPU, context);
  if (spec.startsWith("@/")) spec = new URL(spec.slice(2), SITE).href;
  const local = spec.startsWith(".") || spec.startsWith("file:");
  if (local && !/\.(m?js|ts|tsx|wgsl|json)$/.test(spec)) {
    for (const ext of [".ts", ".tsx"]) {
      try {
        return await next(spec + ext, context);
      } catch {}
    }
  }
  return next(spec, context);
}

export async function load(url, context, next) {
  if (url.endsWith(".wgsl")) {
    const wgsl = await readFile(fileURLToPath(url), "utf8");
    const source = `import { prepareShader } from "@vgpu/wgsl/prepare";\nexport default prepareShader(${JSON.stringify(wgsl)});`;
    return { format: "module", source, shortCircuit: true };
  }
  if (url.endsWith(".ts") && url.startsWith("file:") && !url.includes("/node_modules/")) {
    const src = await readFile(fileURLToPath(url), "utf8");
    const { outputText } = ts.transpileModule(src, {
      compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022, verbatimModuleSyntax: false },
      fileName: fileURLToPath(url),
    });
    return { format: "module", source: outputText, shortCircuit: true };
  }
  return next(url, context);
}
