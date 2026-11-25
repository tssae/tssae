import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { withTempHost } from "@tssae/test-hosts";
import {
  extractBasisEvidence,
  openProject,
  resolveRetainedObjectFields,
  type ObjectFieldShape,
  type ValuePropagationPath,
} from "tssae";

void describe("tssae structured member & rest-destructuring propagation", () => {
  void it("tracks sourcePath, omittedProperties, destructuring step provenance, and wrapper composition independently of step.description", async () => {
    await withTempHost(
      { host: "sdk-queries/row-field-propagation" },
      async (host) => {
        await host.typecheck();

        // Inject intentional omitted-property read after validating the checked host compiles.
        host.replaceText(
          "src/app.ts",
          'sink("omittedSlugAccess", page.slug);',
          'sink("omittedSlugAccess", rest1.slug);',
        );

        const observedByTag = new Map<
          string,
          {
            readonly reachable: boolean;
            readonly path?: ValuePropagationPath;
          }
        >();
        const defaultModeReachable = new Map<string, boolean>();
        let rest1StepLine: number | undefined;
        let destructuredAliasStepLine: number | undefined;

        const session = await openProject({
          cwd: host.rootDir,
          tsconfig: "tsconfig.json",
        });
        try {
          const ctx = session.project();

          const calls = await ctx.findSyntax({
            files: ["src/app.ts"],
            kinds: ["CallExpression"],
          });
          const fetchCall = calls.find((c) => c.text.startsWith("fetchPage("));
          assert.ok(fetchCall);
          const sinkCalls = calls.filter((c) => c.text.startsWith("sink("));
          const resolvedSinks = await ctx.resolveCalls(
            sinkCalls.map((c) => c.ref),
            { select: { location: true, arguments: true } },
          );

          const tagAndArg: {
            readonly tag: string;
            readonly argRef: NonNullable<
              NonNullable<(typeof resolvedSinks)[number]["arguments"]>[number]
            >;
            readonly callLoc: NonNullable<
              (typeof resolvedSinks)[number]["location"]
            >;
          }[] = [];

          const tagRefs = resolvedSinks.map((r) => r.arguments[0]!);
          const tagTexts = await ctx.syntaxOf(tagRefs, {
            select: { text: true },
          });
          for (const [idx, row] of resolvedSinks.entries()) {
            const rawTag = tagTexts[idx]?.text ?? "";
            const tag = rawTag.replaceAll('"', "");
            const argRef = row.arguments?.[1];
            const callLoc = row.location;
            assert.ok(argRef && callLoc);
            tagAndArg.push({ tag, argRef, callLoc });
          }

          const withMembers = await ctx.valuePropagation({
            propagation: "value",
            includeMemberAccess: true,
            from: fetchCall.ref,
            to: tagAndArg.map((entry) => entry.argRef),
          });
          const defaultOnly = await ctx.valuePropagation({
            propagation: "value",
            from: fetchCall.ref,
            to: tagAndArg.map((entry) => entry.argRef),
          });

          for (const [idx, entry] of tagAndArg.entries()) {
            const res = withMembers.results[idx]!;
            const path = res.paths[0];
            observedByTag.set(
              entry.tag,
              path
                ? { reachable: res.reachable, path }
                : { reachable: res.reachable },
            );
            defaultModeReachable.set(
              entry.tag,
              defaultOnly.results[idx]!.reachable,
            );
          }

          const rest1Path = observedByTag.get("rest1")?.path;
          assert.ok(rest1Path);
          rest1StepLine = rest1Path.steps.find(
            (s) =>
              s.kind === "restDestructure" && s.omittedProperties !== undefined,
          )?.toLocation.line;

          const aliasPath = observedByTag.get("destructuredAlias")?.path;
          assert.ok(aliasPath);
          destructuredAliasStepLine = aliasPath.steps.find(
            (s) => s.kind === "destructure",
          )?.toLocation.line;

          const rest1Evidence = extractBasisEvidence([rest1Path]);

          assert.equal(observedByTag.get("direct")?.reachable, true);
          assert.deepEqual(observedByTag.get("direct")?.path?.sourcePath, []);
          assert.deepEqual(
            observedByTag.get("direct")?.path?.omittedProperties,
            [],
          );

          for (const memberTag of [
            "dotProp",
            "bracketProp",
            "destructuredSame",
            "destructuredAlias",
            "survivingWsAccess",
            "wrappedMember",
          ]) {
            const obs = observedByTag.get(memberTag);
            assert.equal(
              obs?.reachable,
              true,
              `Expected ${memberTag} reachable`,
            );
            assert.deepEqual(
              obs?.path?.sourcePath,
              ["workspace"],
              `Unexpected sourcePath for ${memberTag}`,
            );
            assert.deepEqual(
              obs?.path?.omittedProperties,
              [],
              `Unexpected omittedProperties for ${memberTag}`,
            );
          }

          assert.equal(observedByTag.get("nestedOwner")?.reachable, true);
          assert.deepEqual(observedByTag.get("nestedOwner")?.path?.sourcePath, [
            "workspace",
            "owner",
          ]);
          assert.deepEqual(
            observedByTag.get("nestedOwner")?.path?.omittedProperties,
            [],
          );

          assert.equal(observedByTag.get("rest1")?.reachable, true);
          assert.deepEqual(observedByTag.get("rest1")?.path?.sourcePath, []);
          assert.deepEqual(
            observedByTag.get("rest1")?.path?.omittedProperties,
            ["slug"],
          );
          assert.equal(rest1StepLine, 29);

          assert.equal(destructuredAliasStepLine, 26);
          const aliasDestructureStep = observedByTag
            .get("destructuredAlias")
            ?.path?.steps.find((s) => s.kind === "destructure");
          assert.equal(aliasDestructureStep?.property, "workspace");

          assert.equal(observedByTag.get("rest2")?.reachable, true);
          assert.deepEqual(observedByTag.get("rest2")?.path?.sourcePath, []);
          assert.deepEqual(
            observedByTag.get("rest2")?.path?.omittedProperties,
            ["customDomain", "slug"],
          );

          assert.equal(
            observedByTag.get("omittedSlugAccess")?.reachable,
            false,
          );

          assert.equal(observedByTag.get("nestedRest")?.reachable, true);
          assert.deepEqual(observedByTag.get("nestedRest")?.path?.sourcePath, [
            "workspace",
          ]);
          assert.deepEqual(
            observedByTag.get("nestedRest")?.path?.omittedProperties,
            ["plan"],
          );

          assert.equal(defaultModeReachable.get("direct"), true);
          assert.equal(defaultModeReachable.get("rest1"), true);
          assert.equal(defaultModeReachable.get("dotProp"), false);
          assert.equal(defaultModeReachable.get("destructuredAlias"), false);

          assert.ok(
            rest1Evidence.some(
              (ev) => ev.at?.file === "src/app.ts" && ev.at.line === 29,
            ),
            JSON.stringify(rest1Evidence),
          );
        } finally {
          await session.close();
        }

        const rowShape: ObjectFieldShape = {
          fields: ["id", "slug", "customDomain", "workspace"],
          hasDynamicFields: false,
          nested: {
            workspace: {
              fields: ["id", "plan", "owner"],
              hasDynamicFields: false,
              nested: {
                owner: {
                  fields: ["id", "email"],
                  hasDynamicFields: false,
                },
              },
            },
          },
        };

        const mutateDescriptions = (
          path: ValuePropagationPath,
        ): ValuePropagationPath => ({
          ...path,
          steps: path.steps.map((step) => ({
            ...step,
            description: "mutated debug prose without omitting info",
          })),
        });

        const rest2Retained = resolveRetainedObjectFields(
          rowShape,
          mutateDescriptions(observedByTag.get("rest2")!.path!),
        );
        assert.deepEqual(rest2Retained, {
          fields: ["id", "workspace"],
          omittedFields: ["customDomain", "slug"],
          hasDynamicFields: false,
          sourcePath: [],
          matchedNested: false,
        });

        const nestedRestRetained = resolveRetainedObjectFields(
          rowShape,
          mutateDescriptions(observedByTag.get("nestedRest")!.path!),
        );
        assert.deepEqual(nestedRestRetained, {
          fields: ["id", "owner"],
          omittedFields: ["plan"],
          hasDynamicFields: false,
          sourcePath: ["workspace"],
          matchedNested: true,
        });

        const nestedOwnerRetained = resolveRetainedObjectFields(
          rowShape,
          mutateDescriptions(observedByTag.get("nestedOwner")!.path!),
        );
        assert.deepEqual(nestedOwnerRetained, {
          fields: ["id", "email"],
          omittedFields: [],
          hasDynamicFields: false,
          sourcePath: ["workspace", "owner"],
          matchedNested: true,
        });
      },
    );
  });
});
