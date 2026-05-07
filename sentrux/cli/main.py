"""CLI interface for Sentrux."""

import json
import sys
from pathlib import Path

import click

from sentrux.core.analyzer import ProjectAnalyzer
from sentrux.core.rules import RulesEngine
from sentrux.utils.config import ConfigLoader


@click.group()
@click.version_option()
def cli():
    """Sentrux - Structural quality analysis for Python projects."""
    pass


@cli.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--json", "output_json", is_flag=True, help="Output in JSON format")
@click.option(
    "-v",
    "--verbose",
    count=True,
    help="Increase verbosity: -v critical/high, -vv +medium, -vvv +low",
)
def scan(path: str, output_json: bool, verbose: int):
    """Analyze code quality of a Python project."""
    try:
        project_path = Path(path)
        analyzer = ProjectAnalyzer(project_path)
        analysis = analyzer.analyze()

        if output_json:
            click.echo(json.dumps(analysis.to_dict(), indent=2))
        else:
            _print_analysis_summary(analysis, verbose)

    except Exception as e:
        click.echo(f"Error: {e}", err=True)
        raise SystemExit(1)


@cli.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--json", "output_json", is_flag=True, help="Output in JSON format")
def check(path: str, output_json: bool):
    """Check if project meets architectural rules."""
    try:
        project_path = Path(path)

        # Load rules
        rules = ConfigLoader.load_rules(project_path)

        # Analyze
        analyzer = ProjectAnalyzer(project_path)
        analysis = analyzer.analyze()

        # Check rules
        engine = RulesEngine(rules)
        violations = engine.check_violations(analysis)

        if output_json:
            result = {
                "path": str(project_path),
                "quality_score": (
                    analysis.quality_score.to_dict() if analysis.quality_score else None
                ),
                "violations": violations,
                "passed": len(violations) == 0,
            }
            click.echo(json.dumps(result, indent=2))
        else:
            if violations:
                click.echo("❌ Violations found:")
                for violation in violations:
                    click.echo(f"  - {violation}")
                raise SystemExit(1)
            else:
                click.echo("✅ All rules passed")

    except SystemExit:
        raise
    except Exception as e:
        click.echo(f"Error: {e}", err=True)
        raise SystemExit(1)


@cli.command()
@click.argument("path", type=click.Path(exists=True), default=".")
@click.option("--save", is_flag=True, help="Save current metrics as baseline")
def gate(path: str, save: bool):
    """Check quality against baseline or save new baseline."""
    try:
        project_path = Path(path)

        # Analyze
        analyzer = ProjectAnalyzer(project_path)
        analysis = analyzer.analyze()

        if save:
            # Save baseline
            if analysis.quality_score:
                baseline = analysis.quality_score.to_dict()
                ConfigLoader.save_baseline(project_path, baseline)
                click.echo(f"Baseline saved: {analysis.quality_score.overall_score}/10000")
        else:
            # Compare against baseline
            baseline = ConfigLoader.load_baseline(project_path)

            if not baseline:
                click.echo("No baseline found. Use --save to create one.")
                raise SystemExit(1)

            if analysis.quality_score:
                current_score = analysis.quality_score.overall_score
                baseline_score = baseline.get("overall_score", 0)

                click.echo(f"Baseline:  {baseline_score}/10000")
                click.echo(f"Current:   {current_score}/10000")

                if current_score < baseline_score:
                    diff = baseline_score - current_score
                    click.echo(f"⚠️  Regression: -{diff} points")
                    raise SystemExit(1)
                elif current_score > baseline_score:
                    improvement = current_score - baseline_score
                    click.echo(f"✅ Improvement: +{improvement} points")
                else:
                    click.echo("✅ No change")

    except SystemExit:
        raise
    except Exception as e:
        click.echo(f"Error: {e}", err=True)
        raise SystemExit(1)


_PRIORITY_ICON = {"critical": "[CRITICAL]", "high": "[HIGH]", "medium": "[MEDIUM]", "low": "[LOW]"}
_PRIORITY_ORDER = {"critical": 0, "high": 1, "medium": 2, "low": 3}


def _print_analysis_summary(analysis, verbose: int = 0) -> None:
    """Print analysis summary. Verbosity: 0=worst only, 1=high+, 2=medium+, 3=all."""
    score = analysis.quality_score
    report = analysis.detailed_report

    if score:
        click.echo(f"Quality Score: {score.overall_score}/10000")
        if verbose >= 1:
            click.echo(f"  Modularity:  {score.modularity:.2f}")
            click.echo(f"  Acyclicity:  {score.acyclicity:.2f}")
            click.echo(f"  Depth:       {score.depth:.2f}")
            click.echo(f"  Equality:    {score.equality:.2f}")
            click.echo(f"  Redundancy:  {score.redundancy:.2f}")

    if report and report.violations:
        violations_sorted = sorted(
            report.violations, key=lambda v: _PRIORITY_ORDER.get(v.priority, 3)
        )

        if verbose == 0:
            v = violations_sorted[0]
            icon = _PRIORITY_ICON.get(v.priority, "[INFO]")
            click.echo(f"\n{icon} {v.file_path} — {v.metric.capitalize()}")
            click.echo(f"  {v.description}")
            remaining = len(violations_sorted) - 1
            tail = (
                f"  ({remaining} more violation{'s' if remaining > 1 else ''}; use -v to see more)"
            )
            click.echo(tail) if remaining else None
        else:
            allowed = (
                {"critical", "high", "medium", "low"}
                if verbose >= 3
                else {"critical", "high", "medium"} if verbose >= 2 else {"critical", "high"}
            )
            shown = [v for v in violations_sorted if v.priority in allowed]
            click.echo(f"\nViolations ({len(shown)} shown, {len(violations_sorted)} total):")
            for v in shown:
                icon = _PRIORITY_ICON.get(v.priority, "[INFO]")
                click.echo(f"\n{icon} {v.file_path} — {v.metric.capitalize()}")
                click.echo(f"  {v.description}")

    if analysis.rules_violations:
        click.echo(f"\nRule violations: {len(analysis.rules_violations)}")
        for rv in analysis.rules_violations:
            click.echo(f"  - {rv}")


def main():
    """Entry point for the CLI."""
    if "--mcp" in sys.argv:
        from sentrux.mcp.server import run_mcp_server
        run_mcp_server()
    else:
        cli()


if __name__ == "__main__":
    main()
