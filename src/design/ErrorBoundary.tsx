import { Component, type ReactNode } from "react";
import { Button, EmptyState } from "./components";

/**
 * Catches a render error so one broken component shows a message instead of
 * blanking the whole window. Matters most in dev, where an agent editing tab's
 * own source can hot-reload a half-written file.
 */
export class ErrorBoundary extends Component<{ children: ReactNode; label?: string }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: { componentStack?: string | null }) {
    console.error(`[tab] ${this.props.label ?? "view"} crashed`, error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="error-boundary">
        <EmptyState icon="alert" title={`${this.props.label ?? "This view"} hit an error`}>
          {error.message}
        </EmptyState>
        <div className="error-boundary-actions">
          <Button icon="refresh" onClick={() => this.setState({ error: null })}>Try again</Button>
          <Button variant="ghost" onClick={() => location.reload()}>Reload tab</Button>
        </div>
      </div>
    );
  }
}
