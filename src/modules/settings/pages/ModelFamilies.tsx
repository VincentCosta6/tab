import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, FormRow, Section, TextInput } from "../../../design/components";
import { api } from "../../../lib/api";
import { useHarnesses } from "../../../lib/hooks";
import type { AgentKind, ModelFamily } from "../../../lib/types";
import { ModelMenu } from "../../work/ModelMenu";

const BUILT_IN = ["fast", "balanced", "deep"];

/**
 * Model families: named, ordered lists of models a workflow's agent step
 * can ask for. The runner that takes the step uses the first one it has and
 * moves on to the next when a model is unavailable, rate limited or out of
 * plan. Saved straight away, and synced to every runner.
 */
export function ModelFamiliesSettings() {
  const qc = useQueryClient();
  const families = useQuery({ queryKey: ["model-families"], queryFn: api.modelFamilies });
  const { data: harnesses = [] } = useHarnesses();
  const [error, setError] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const done = (p: Promise<ModelFamily[]>) =>
    p.then((all) => { setError(null); qc.setQueryData(["model-families"], all); }).catch((e) => setError(String(e)));
  const save = (f: ModelFamily) => done(api.modelFamilySave(f));
  const add = () => {
    const name = newName.trim();
    const id = name.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
    if (!id) return;
    const first = harnesses[0]?.id ?? "claude_code";
    save({ id, name, candidates: [{ harness: first, model: "" }] }).then(() => setNewName(""));
  };
  return (
    <Section title="Model families" description="A workflow's agent step can ask for a family instead of one model. The runner that takes it uses the first model it has, and moves on to the next if that model is unavailable, rate limited or out of plan.">
      <Card>
        {(families.data ?? []).map((f) => (
          <FamilyRow key={f.id} family={f} onSave={save} onDelete={() => done(api.modelFamilyDelete(f.id))} />
        ))}
        <FormRow label="New family">
          <div className="family-add">
            <TextInput placeholder="e.g. Cheap reviews" value={newName} onChange={(e) => setNewName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && add()} />
            <Button size="sm" onClick={add} disabled={!newName.trim()}>Add</Button>
          </div>
        </FormRow>
        {error && <small className="text-danger" role="alert">{error}</small>}
      </Card>
    </Section>
  );
}

function FamilyRow({ family, onSave, onDelete }: { family: ModelFamily; onSave: (f: ModelFamily) => void; onDelete: () => void }) {
  const builtIn = BUILT_IN.includes(family.id);
  const set = (candidates: ModelFamily["candidates"]) => onSave({ ...family, candidates });
  const c = family.candidates;
  const move = (i: number, by: number) => {
    const next = [...c];
    [next[i], next[i + by]] = [next[i + by], next[i]];
    set(next);
  };
  return (
    <FormRow
      label={family.name || family.id}
      help={<>{builtIn ? <Badge>Built in</Badge> : <code>{family.id}</code>} Tried top to bottom.</>}
    >
      <div className="family-list">
        {c.map((cand, i) => (
          <div key={i} className="family-item">
            <span className="subtle">{i + 1}.</span>
            <ModelMenu
              variant="field"
              value={{ agent: cand.harness as AgentKind, model: cand.model || null }}
              onChange={(p) => set(c.map((x, j) => (j === i ? { harness: p.agent, model: p.model ?? "" } : x)))}
            />
            <Button size="sm" variant="ghost" icon="arrow-up" disabled={i === 0} onClick={() => move(i, -1)} aria-label="Move up" />
            <Button size="sm" variant="ghost" icon="arrow-down" disabled={i === c.length - 1} onClick={() => move(i, 1)} aria-label="Move down" />
            <Button size="sm" variant="ghost" icon="x" disabled={c.length === 1} onClick={() => set(c.filter((_, j) => j !== i))} aria-label="Remove" />
          </div>
        ))}
        <div className="family-actions">
          <Button size="sm" icon="plus" onClick={() => set([...c, { harness: c[c.length - 1]?.harness ?? "claude_code", model: "" }])}>Add a model</Button>
          <Button size="sm" variant="ghost" onClick={onDelete}>{builtIn ? "Reset" : "Delete"}</Button>
        </div>
      </div>
    </FormRow>
  );
}
