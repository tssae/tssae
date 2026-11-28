import assert from "node:assert/strict";
import { test } from "node:test";

import { withAcceptanceHost } from "@tssae/acceptance";

const consumerTsconfig = JSON.stringify(
  {
    compilerOptions: {
      strict: true,
      exactOptionalPropertyTypes: true,
      noUncheckedIndexedAccess: true,
      target: "ES2024",
      module: "NodeNext",
      moduleResolution: "NodeNext",
      erasableSyntaxOnly: true,
      rewriteRelativeImportExtensions: true,
      types: ["node"],
      noEmit: true,
    },
    include: ["consumer.ts"],
  },
  null,
  2,
);

const consumerSource = `import assert from "node:assert/strict";

import {
  TssaeError,
  getEngineVersion,
  openProject,
  type CallRef,
  type SignatureRef,
} from "tssae";

assert.equal(process.versions.node, "24.0.0");

const engineVersion = await getEngineVersion();
assert.match(engineVersion, /^\\d+\\.\\d+\\.\\d+(?:[-+][\\w.+-]+)?$/u);

const session = await openProject({
  cwd: process.cwd(),
  tsconfig: "tsconfig.json",
});

let textCallSig: SignatureRef;
let numCallSig: SignatureRef;
let wrapStrSig: SignatureRef;
let wrapNumSig: SignatureRef;

let capturedProject: ReturnType<typeof session.project>;
try {
  assert.equal(session.projects.length, 1);
  const project = session.project();
  capturedProject = project;

  const calls = await project.findSyntax({
    files: ["src/caller.ts"],
    kinds: ["CallExpression"],
  });
  assert.equal(calls.length, 4);

  const resolvedCalls = await project.resolveCalls(
    calls.map((call) => call.ref),
    {
      select: {
        location: true,
        calleeDeclaration: true,
        signature: true,
      },
    },
  );
  const callRefs = resolvedCalls
    .map((row) => row.call)
    .filter((ref): ref is CallRef => ref !== null);
  assert.equal(callRefs.length, 4);

  const projectedCalls = await project.projectCalls(callRefs, {
    select: {
      location: true,
      calleeDeclaration: true,
      signature: true,
      instantiatedParameterTypes: true,
      returnType: true,
    },
  });

  const [textCall, numCall, wrapStrCall, wrapNumCall] = projectedCalls;
  assert.ok(textCall && numCall && wrapStrCall && wrapNumCall);
  assert.ok(textCall.signature && numCall.signature && wrapStrCall.signature && wrapNumCall.signature);
  textCallSig = textCall.signature;
  numCallSig = numCall.signature;
  wrapStrSig = wrapStrCall.signature;
  wrapNumSig = wrapNumCall.signature;

  assert.equal(textCall.instantiatedParameterTypes[0]?.type.typeText, "string");
  assert.match(textCall.returnType?.typeText ?? "", /kind:\\s*['"]text['"]/u);

  assert.equal(numCall.instantiatedParameterTypes[0]?.type.typeText, "number");
  assert.match(numCall.returnType?.typeText ?? "", /kind:\\s*['"]num['"]/u);

  assert.equal(wrapStrCall.instantiatedParameterTypes[0]?.type.typeText, "string");
  assert.equal(wrapStrCall.instantiatedParameterTypes[1]?.type.typeText, "string");

  assert.equal(wrapNumCall.instantiatedParameterTypes[0]?.type.typeText, "number");
  assert.equal(wrapNumCall.instantiatedParameterTypes[1]?.type.typeText, "string");

  const sigRows = await project.projectSignatures(
    [textCallSig, numCallSig, wrapStrSig, wrapNumSig],
    {
      select: {
        location: true,
        declaration: true,
        isInstantiated: true,
        instantiatedParameterTypes: true,
        returnType: true,
        parameters: true,
        signatureText: true,
      },
    },
  );

  const [textSigRow, numSigRow, wrapStrSigRow, wrapNumSigRow] = sigRows;
  assert.ok(textSigRow && numSigRow && wrapStrSigRow && wrapNumSigRow);

  assert.equal(textSigRow.location?.file, "src/overloads.ts");
  assert.equal(textSigRow.location?.line, 1);
  assert.equal(textSigRow.isInstantiated, false);
  assert.equal(textSigRow.instantiatedParameterTypes[0]?.type.typeText, "string");

  assert.equal(numSigRow.location?.file, "src/overloads.ts");
  assert.equal(numSigRow.location?.line, 2);
  assert.equal(numSigRow.isInstantiated, false);
  assert.equal(numSigRow.instantiatedParameterTypes[0]?.type.typeText, "number");

  assert.equal(wrapStrSigRow.location?.file, "src/overloads.ts");
  assert.equal(wrapStrSigRow.location?.line, 12);
  assert.equal(wrapStrSigRow.isInstantiated, true);
  assert.equal(wrapStrSigRow.parameters.length, 2);
  assert.equal(wrapStrSigRow.instantiatedParameterTypes[0]?.type.typeText, "string");
  assert.equal(wrapStrSigRow.instantiatedParameterTypes[1]?.type.typeText, "string");
  assert.match(wrapStrSigRow.returnType.typeText, /item:\\s*string/u);

  assert.equal(wrapNumSigRow.location?.file, "src/overloads.ts");
  assert.equal(wrapNumSigRow.location?.line, 12);
  assert.equal(wrapNumSigRow.isInstantiated, true);
  assert.equal(wrapNumSigRow.instantiatedParameterTypes[0]?.type.typeText, "number");
  assert.equal(wrapNumSigRow.instantiatedParameterTypes[1]?.type.typeText, "string");
  assert.match(wrapNumSigRow.returnType.typeText, /item:\\s*number/u);

  const calleeDecls = await project.resolveDeclarations(
    [textSigRow.signature, numSigRow.signature],
    { select: { name: true, location: true } },
  );
  assert.equal(calleeDecls[0]?.name, "parseInput");
  assert.equal(calleeDecls[0]?.location?.file, "src/overloads.ts");
  assert.equal(calleeDecls[0]?.location?.line, 1);
  assert.equal(calleeDecls[1]?.name, "parseInput");
  assert.equal(calleeDecls[1]?.location?.file, "src/overloads.ts");
  assert.equal(calleeDecls[1]?.location?.line, 2);

  process.stdout.write(
    JSON.stringify({
      nodeVersion: process.versions.node,
      engineVersion,
      textOverloadLine: textSigRow.location?.line,
      numOverloadLine: numSigRow.location?.line,
      wrapStrParam0: wrapStrSigRow.instantiatedParameterTypes[0]?.type.typeText,
      wrapNumParam0: wrapNumSigRow.instantiatedParameterTypes[0]?.type.typeText,
    }) + "\\n",
  );
} finally {
  await session.close();
}

assert.throws(
  () => session.project(),
  (err: unknown) => err instanceof TssaeError && err.code === "SESSION_CLOSED",
);
await assert.rejects(
  () => capturedProject.findSyntax({ kinds: ["CallExpression"] }),
  (err: unknown) => err instanceof TssaeError && err.code === "SESSION_CLOSED",
);
`;

void test("installs candidate tssae out of repo, typechecks consumer .d.ts, queries cross-file overloads, and exits cleanly", async (t) => {
  await withAcceptanceHost(
    {
      hostFixture: "sdk-queries/cross-file-overloads",
      files: {
        "package.json": JSON.stringify(
          {
            name: "tssae-standalone-overload-consumer",
            private: true,
            type: "module",
            devDependencies: {
              "@types/node": "^26.0.0",
              typescript: "^7.0.2",
            },
          },
          null,
          2,
        ),
        "consumer.tsconfig.json": consumerTsconfig,
        "consumer.ts": consumerSource,
      },
    },
    async (host) => {
      const tssaeArtifact = host.artifact("tssae");
      assert.ok(tssaeArtifact.binarySha256);
      assert.deepEqual(tssaeArtifact.manifest.repository, {
        type: "git",
        url: "git+https://github.com/tssae/tssae.git",
        directory: "packages/tssae",
      });
      assert.equal(host.audit.tssaeVersion, tssaeArtifact.version);
      assert.equal(host.audit.engineBinarySha256, tssaeArtifact.binarySha256);
      assert.equal(host.audit.hasSourceConditionInExports, false);
      assert.ok(host.audit.auditedSymlinkCount >= 0);

      const tscResult = await host.runTsc("consumer.tsconfig.json");
      assert.equal(
        tscResult.status,
        0,
        `Consumer tsc --noEmit failed:\n${tscResult.stdout}\n${tscResult.stderr}`,
      );
      assert.equal(tscResult.isolation?.controlProbeBlocked, true);

      const runResult = await host.runNode(["consumer.ts"]);
      assert.equal(
        runResult.status,
        0,
        `Consumer script failed (status=${String(runResult.status)}):\nstdout:\n${runResult.stdout}\nstderr:\n${runResult.stderr}`,
      );
      assert.equal(runResult.timedOut, false);
      assert.equal(runResult.isolation?.controlProbeBlocked, true);

      const summary = JSON.parse(runResult.stdout.trim()) as {
        readonly nodeVersion: string;
        readonly engineVersion: string;
        readonly textOverloadLine: number;
        readonly numOverloadLine: number;
        readonly wrapStrParam0: string;
        readonly wrapNumParam0: string;
      };
      assert.equal(summary.nodeVersion, "24.0.0");
      assert.equal(summary.textOverloadLine, 1);
      assert.equal(summary.numOverloadLine, 2);
      assert.equal(summary.wrapStrParam0, "string");
      assert.equal(summary.wrapNumParam0, "number");

      t.diagnostic(
        `Packed tssae ${tssaeArtifact.version}; engine ${summary.engineVersion} SHA-256 ${host.audit.engineBinarySha256}; isolation ${runResult.isolation?.mode}`,
      );
    },
  );
});
