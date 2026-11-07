import { useQuery } from "@tanstack/react-query";
import { Button, Card, FormRow, Section } from "../../../design/components";
import { api } from "../../../lib/api";

export function McpPage() {
  const mcp = useQuery({ queryKey: ["mcp"], queryFn: api.mcpInfo });
  return (
    <Section title="MCP server" description="Agents tab starts are connected automatically. Point any other MCP client here to let it search your queue, file what it finds, and create tasks.">
      <Card>
        <FormRow label="URL">
          <code className="mono">{mcp.data?.url ?? "not running"}</code>
        </FormRow>
        <FormRow label="Token" help="Send as Authorization: Bearer …">
          <Button size="sm" icon="copy" onClick={() => mcp.data && navigator.clipboard.writeText(mcp.data.token)}>Copy token</Button>
        </FormRow>
      </Card>
    </Section>
  );
}
