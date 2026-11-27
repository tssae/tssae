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
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import {
  TssaeError,
  openProject,
  type CallRef,
  type DeclarationRef,
  type ProjectContext,
  type SignatureRef,
  type SyntaxRef,
  type TssaeErrorCode,
  type TypeRef,
} from "tssae";

type NormalizedCallSemantics = {
  readonly callLine: number;
  readonly calleeDeclName: string | null;
  readonly calleeDeclFile: string | null;
  readonly calleeDeclLine: number | null;
  readonly signatureFile: string | null;
  readonly signatureLine: number | null;
  readonly signatureText: string;
  readonly isInstantiated: boolean;
  readonly parameters: readonly {
    readonly name: string | null;
    readonly optional: boolean;
    readonly rest: boolean;
  }[];
  readonly instantiatedParameterTypes: readonly {
    readonly index: number;
    readonly parameterName: string | null;
    readonly isRest: boolean;
    readonly typeText: string;
    readonly isUnion: boolean;
    readonly isIntersection: boolean;
    readonly isLiteral: boolean;
  }[];
  readonly returnTypeText: string;
  readonly resolvedDeclName: string | null;
  readonly resolvedDeclFile: string | null;
  readonly resolvedDeclLine: number | null;
};

type CapturedQuerySnapshot = {
  readonly semantics: readonly NormalizedCallSemantics[];
  readonly syntaxRef: SyntaxRef;
  readonly callRef: CallRef;
  readonly signatureRef: SignatureRef;
  readonly declarationRef: DeclarationRef;
  readonly typeRef: TypeRef;
};

async function queryCallerSemantics(
  project: ProjectContext,
): Promise<CapturedQuerySnapshot> {
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

  const sigRefs = projectedCalls.map((row) => {
    assert.ok(row.signature);
    return row.signature;
  });

  const sigRows = await project.projectSignatures(sigRefs, {
    select: {
      location: true,
      declaration: true,
      isInstantiated: true,
      instantiatedParameterTypes: true,
      returnType: true,
      parameters: true,
      signatureText: true,
    },
  });

  const declRefs = sigRows.map((row) => {
    assert.ok(row.declaration);
    return row.declaration;
  });

  const calleeDecls = await project.resolveDeclarations(sigRefs, {
    select: { name: true, location: true },
  });
  const resolvedCallDecls = await project.resolveDeclarations(callRefs, {
    select: { name: true, location: true },
  });

  const semantics: NormalizedCallSemantics[] = projectedCalls.map(
    (callRow, index) => {
      const sigRow = sigRows[index];
      const calleeDecl = calleeDecls[index];
      const sigDecl = resolvedCallDecls[index];
      assert.ok(sigRow && calleeDecl && sigDecl);
      assert.equal(
        callRow.instantiatedParameterTypes[0]?.type.typeText,
        sigRow.instantiatedParameterTypes[0]?.type.typeText,
      );
      return {
        callLine: callRow.location?.line ?? -1,
        calleeDeclName: sigDecl.name,
        calleeDeclFile: sigDecl.location?.file ?? null,
        calleeDeclLine: sigDecl.location?.line ?? null,
        signatureFile: sigRow.location?.file ?? null,
        signatureLine: sigRow.location?.line ?? null,
        signatureText: sigRow.signatureText,
        isInstantiated: sigRow.isInstantiated,
        parameters: sigRow.parameters.map((param) => ({
          name: param.name,
          optional: param.optional,
          rest: param.rest,
        })),
        instantiatedParameterTypes: sigRow.instantiatedParameterTypes.map(
          (param) => ({
            index: param.index,
            parameterName: param.parameterName,
            isRest: param.isRest,
            typeText: param.type.typeText,
            isUnion: param.type.isUnion,
            isIntersection: param.type.isIntersection,
            isLiteral: param.type.isLiteral,
          }),
        ),
        returnTypeText: sigRow.returnType.typeText,
        resolvedDeclName: calleeDecl.name,
        resolvedDeclFile: calleeDecl.location?.file ?? null,
        resolvedDeclLine: calleeDecl.location?.line ?? null,
      };
    },
  );

  const firstSyntax = calls[0];
  const firstCall = projectedCalls[0];
  const firstSig = sigRows[0];
  const firstDecl = declRefs[0];
  assert.ok(firstSyntax && firstCall && firstSig && firstDecl);
  assert.ok(firstCall.returnType);

  return {
    semantics,
    syntaxRef: firstSyntax.ref,
    callRef: firstCall.call,
    signatureRef: firstSig.signature,
    declarationRef: firstDecl,
    typeRef: firstCall.returnType.ref,
  };
}

function assertTssaeErrorCode(err: unknown, expected: TssaeErrorCode): boolean {
  if (!(err instanceof TssaeError)) {
    return false;
  }
  const code: TssaeErrorCode = err.code;
  return code === expected;
}

assert.equal(process.versions.node, "24.0.0");

const session = await openProject({
  cwd: process.cwd(),
  tsconfig: "tsconfig.json",
});

let freshSession: Awaited<ReturnType<typeof openProject>> | undefined;
let updatedProjectRef: ProjectContext | undefined;

try {
  // 1. Initial query on unmodified multi-file overload fixture
  const initialSnapshotId = session.snapshotId;
  const initialDigest = session.snapshotDigest;
  const initialProject = session.project();
  const initialCaptured = await queryCallerSemantics(initialProject);

  const initialParseCall = initialCaptured.semantics[0];
  const initialWrapCall = initialCaptured.semantics[2];
  assert.ok(initialParseCall && initialWrapCall);
  assert.equal(initialParseCall.signatureFile, "src/overloads.ts");
  assert.equal(initialParseCall.signatureLine, 1);
  assert.equal(initialParseCall.instantiatedParameterTypes[0]?.typeText, "string");
  assert.match(initialParseCall.returnTypeText, /kind:\\s*['"]text['"]/u);

  assert.equal(initialWrapCall.signatureFile, "src/overloads.ts");
  assert.equal(initialWrapCall.signatureLine, 12);
  assert.equal(initialWrapCall.isInstantiated, true);
  assert.equal(initialWrapCall.instantiatedParameterTypes[0]?.typeText, "string");

  // 2. Edit host source file on disk so calls switch overload & instantiated type
  const callerPath = join(process.cwd(), "src/caller.ts");
  const beforeSource = readFileSync(callerPath, "utf8");
  const afterSource = beforeSource
    .replace('parseInput("hello")', "parseInput(99)")
    .replace('wrap(input, "str")', 'wrap(true, "bool")');
  assert.notEqual(afterSource, beforeSource);
  writeFileSync(callerPath, afterSource, "utf8");

  const updateResult = await session.update({
    changed: ["src/caller.ts"],
  });
  assert.equal(updateResult.previousSnapshotId, initialSnapshotId);
  assert.notEqual(updateResult.snapshotId, initialSnapshotId);
  assert.equal(session.snapshotId, updateResult.snapshotId);
  assert.notEqual(session.snapshotDigest, initialDigest);
  assert.ok(updateResult.recomputation.recomputedFiles.includes("src/caller.ts"));
  assert.ok(
    updateResult.recomputation.invalidatedBy.includes("changed:src/caller.ts"),
  );

  const updatedProject = session.project();
  updatedProjectRef = updatedProject;
  const updatedCaptured = await queryCallerSemantics(updatedProject);

  const updatedParseCall = updatedCaptured.semantics[0];
  const updatedWrapCall = updatedCaptured.semantics[2];
  assert.ok(updatedParseCall && updatedWrapCall);
  assert.equal(updatedParseCall.signatureFile, "src/overloads.ts");
  assert.equal(updatedParseCall.signatureLine, 2);
  assert.equal(updatedParseCall.resolvedDeclName, "parseInput");
  assert.equal(updatedParseCall.resolvedDeclLine, 2);
  assert.equal(updatedParseCall.instantiatedParameterTypes[0]?.typeText, "number");
  assert.match(updatedParseCall.returnTypeText, /kind:\\s*['"]num['"]/u);

  assert.equal(updatedWrapCall.signatureFile, "src/overloads.ts");
  assert.equal(updatedWrapCall.signatureLine, 12);
  assert.equal(updatedWrapCall.isInstantiated, true);
  assert.equal(updatedWrapCall.instantiatedParameterTypes[0]?.typeText, "boolean");
  assert.match(updatedWrapCall.returnTypeText, /item:\\s*boolean/u);

  // 3. Open an independent fresh session on the exact same final files and compare semantics
  freshSession = await openProject({
    cwd: process.cwd(),
    tsconfig: "tsconfig.json",
  });
  assert.equal(freshSession.snapshotDigest, session.snapshotDigest);
  const freshCaptured = await queryCallerSemantics(freshSession.project());
  assert.deepEqual(updatedCaptured.semantics, freshCaptured.semantics);

  // 4. Verify stale snapshot context and stale ProgramRefs from snapshot 1 fail with public error codes
  await assert.rejects(
    () => initialProject.findSyntax({ kinds: ["CallExpression"] }),
    (err: unknown) => assertTssaeErrorCode(err, "STALE_SNAPSHOT"),
  );

  await assert.rejects(
    () =>
      updatedProject.projectCalls([initialCaptured.callRef], {
        select: { location: true },
      }),
    (err: unknown) => assertTssaeErrorCode(err, "STALE_REFERENCE"),
  );
  await assert.rejects(
    () =>
      updatedProject.projectSignatures([initialCaptured.signatureRef], {
        select: { returnType: true },
      }),
    (err: unknown) => assertTssaeErrorCode(err, "STALE_REFERENCE"),
  );
  await assert.rejects(
    () =>
      updatedProject.resolveDeclarations([initialCaptured.syntaxRef], {
        select: { location: true },
      }),
    (err: unknown) => assertTssaeErrorCode(err, "STALE_REFERENCE"),
  );
  await assert.rejects(
    () =>
      updatedProject.syntaxOf([initialCaptured.declarationRef], {
        select: { location: true },
      }),
    (err: unknown) => assertTssaeErrorCode(err, "STALE_REFERENCE"),
  );
  await assert.rejects(
    () =>
      updatedProject.projectTypes([initialCaptured.typeRef], {
        select: { typeText: true },
      }),
    (err: unknown) => assertTssaeErrorCode(err, "STALE_REFERENCE"),
  );

  process.stdout.write(
    JSON.stringify({
      nodeVersion: process.versions.node,
      initialSnapshotId,
      updatedSnapshotId: updateResult.snapshotId,
      initialOverloadLine: initialParseCall.signatureLine,
      updatedOverloadLine: updatedParseCall.signatureLine,
      initialWrapParam0: initialWrapCall.instantiatedParameterTypes[0]?.typeText,
      updatedWrapParam0: updatedWrapCall.instantiatedParameterTypes[0]?.typeText,
      semanticParityMatched: true,
    }) + "\\n",
  );
} finally {
  // 5. Release both sessions cleanly
  await freshSession?.close();
  await session.close();
}

assert.throws(
  () => session.project(),
  (err: unknown) => assertTssaeErrorCode(err, "SESSION_CLOSED"),
);
if (updatedProjectRef !== undefined) {
  const closedProject = updatedProjectRef;
  await assert.rejects(
    () => closedProject.findSyntax({ kinds: ["CallExpression"] }),
    (err: unknown) => assertTssaeErrorCode(err, "SESSION_CLOSED"),
  );
}
`;

void test("updates snapshot after editing host source, matches fresh openProject semantics, rejects stale snapshot refs, and closes cleanly out of repo", async (t) => {
  await withAcceptanceHost(
    {
      hostFixture: "sdk-queries/cross-file-overloads",
      files: {
        "package.json": JSON.stringify(
          {
            name: "tssae-snapshot-updates-consumer",
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
      assert.equal(host.audit.tssaeVersion, tssaeArtifact.version);
      assert.equal(host.audit.engineBinarySha256, tssaeArtifact.binarySha256);
      assert.equal(host.audit.hasSourceConditionInExports, false);

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
        readonly initialSnapshotId: number;
        readonly updatedSnapshotId: number;
        readonly initialOverloadLine: number;
        readonly updatedOverloadLine: number;
        readonly initialWrapParam0: string;
        readonly updatedWrapParam0: string;
        readonly semanticParityMatched: boolean;
      };
      assert.equal(summary.nodeVersion, "24.0.0");
      assert.notEqual(summary.updatedSnapshotId, summary.initialSnapshotId);
      assert.equal(summary.initialOverloadLine, 1);
      assert.equal(summary.updatedOverloadLine, 2);
      assert.equal(summary.initialWrapParam0, "string");
      assert.equal(summary.updatedWrapParam0, "boolean");
      assert.equal(summary.semanticParityMatched, true);

      t.diagnostic(
        `Packed tssae ${tssaeArtifact.version}; snapshot ${String(summary.initialSnapshotId)} -> ${String(summary.updatedSnapshotId)}; overload line ${String(summary.initialOverloadLine)} -> ${String(summary.updatedOverloadLine)}; isolation ${runResult.isolation?.mode}`,
      );
    },
  );
});
