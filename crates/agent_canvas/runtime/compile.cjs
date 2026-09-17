const esbuild = require('./esbuild/lib/main.js');
const ts = require('./typescript/typescript.js');
const path = require('node:path');
const modules = require('./modules.json');

async function compile(source) {
  const sourcePath = path.join(__dirname, 'artifact.canvas.tsx');
  const sdkPath = path.join(__dirname, 'sdk.d.ts');
  const options = { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, jsx: ts.JsxEmit.ReactJSX, strict: true, noEmit: true, skipLibCheck: true, types: [], lib: ['lib.es2022.d.ts', 'lib.dom.d.ts'] };
  const host = ts.createCompilerHost(options);
  const originalRead = host.readFile.bind(host);
  const originalExists = host.fileExists.bind(host);
  host.readFile = file => file === sourcePath ? source : originalRead(file);
  host.fileExists = file => file === sourcePath || originalExists(file);
  const resolutions = {
    '@zed/canvas': sdkPath,
    react: path.join(__dirname, 'types/react/index.d.ts'),
    'react/jsx-runtime': path.join(__dirname, 'types/react/jsx-runtime.d.ts'),
    'react/jsx-dev-runtime': path.join(__dirname, 'types/react/jsx-dev-runtime.d.ts'),
    csstype: path.join(__dirname, 'types/csstype/index.d.ts'),
  };
  host.resolveModuleNames = (names, containingFile) => names.map(name => resolutions[name] ? { resolvedFileName: resolutions[name], extension: ts.Extension.Dts } : containingFile === sourcePath ? undefined : ts.resolveModuleName(name, containingFile, options, host).resolvedModule);
  const file = ts.createSourceFile(sourcePath, source, ts.ScriptTarget.ES2022, true, ts.ScriptKind.TSX);
  for (const statement of file.statements) {
    if ((ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement)) && statement.moduleSpecifier && statement.moduleSpecifier.text !== '@zed/canvas') {
      throw new Error('Unsupported Canvas import: ' + statement.moduleSpecifier.text + '. Import only from @zed/canvas.');
    }
  }
  if (!file.statements.some(statement => ts.isExportAssignment(statement) || statement.modifiers?.some(modifier => modifier.kind === ts.SyntaxKind.DefaultKeyword))) throw new Error('Canvas must default-export a React component.');
  const program = ts.createProgram([sourcePath, sdkPath], options, host);
  const diagnostics = ts.getPreEmitDiagnostics(program);
  if (diagnostics.length) {
    const messages = diagnostics.map(diagnostic => {
      const position = diagnostic.file?.getLineAndCharacterOfPosition(diagnostic.start ?? 0);
      return (position ? 'L' + (position.line + 1) + ':' + (position.character + 1) + ' ' : '') + 'TS' + diagnostic.code + ': ' + ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n');
    });
    throw new Error('Canvas TypeScript check: ' + diagnostics.length + ' issue(s)\n' + messages.join('\n'));
  }
  const result = await esbuild.build({
    stdin: { contents: source, loader: 'tsx', sourcefile: 'artifact.canvas.tsx' },
    bundle: true, write: false, format: 'iife', globalName: 'CanvasModule',
    jsx: 'automatic', platform: 'browser', target: 'safari15',
    sourcemap: 'inline', logLevel: 'silent',
    plugins: [{ name: 'canvas-imports', setup(build) {
      build.onResolve({ filter: /.*/ }, args => {
        if (!Object.hasOwn(modules, args.path)) return { errors: [{ text: 'Unsupported Canvas import: ' + args.path }] };
        return { path: args.path, namespace: 'canvas-runtime' };
      });
      build.onLoad({ filter: /.*/, namespace: 'canvas-runtime' }, args => ({
        contents: 'const runtime = globalThis.__zedCanvasModules[' + JSON.stringify(args.path) + '];\n' + modules[args.path].map(name => name === 'default' ? 'export default runtime.default;' : 'export const ' + name + ' = runtime[' + JSON.stringify(name) + '];').join('\n'),
      }));
    } }],
  });
  return result.outputFiles[0].text;
}

module.exports = { compile };
if (require.main === module) {
  (async () => {
    let source = '';
    for await (const chunk of process.stdin) source += chunk;
    try { process.stdout.write(await compile(source)); }
    catch (error) {
      process.stderr.write(error.errors ? (await esbuild.formatMessages(error.errors, { kind: 'error', color: false })).join('\n') : String(error));
      process.exitCode = 1;
    } finally { esbuild.stop(); }
  })();
}
