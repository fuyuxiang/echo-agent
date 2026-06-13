#!/usr/bin/env python3
"""System health check using psutil."""

import argparse
import json
import sys

try:
    import psutil
except ImportError:
    sys.exit("Install: pip install psutil")

from datetime import datetime


def check_cpu():
    return {"percent": psutil.cpu_percent(interval=1), "cores": psutil.cpu_count(), "freq_mhz": psutil.cpu_freq().current if psutil.cpu_freq() else 0}


def check_memory():
    m = psutil.virtual_memory()
    return {"total_gb": round(m.total / 1e9, 1), "used_gb": round(m.used / 1e9, 1), "percent": m.percent}


def check_disk():
    d = psutil.disk_usage("/")
    return {"total_gb": round(d.total / 1e9, 1), "used_gb": round(d.used / 1e9, 1), "percent": round(d.percent, 1)}


def check_processes(top_n=5):
    procs = []
    for p in psutil.process_iter(["pid", "name", "cpu_percent", "memory_info"]):
        try:
            info = p.info
            procs.append({"pid": info["pid"], "name": info["name"], "cpu": info["cpu_percent"] or 0, "mem_mb": round((info["memory_info"].rss if info["memory_info"] else 0) / 1e6, 1)})
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    procs.sort(key=lambda x: x["cpu"], reverse=True)
    return procs[:top_n]


def full_report():
    cpu = check_cpu()
    mem = check_memory()
    disk = check_disk()
    load = psutil.getloadavg()
    boot = datetime.fromtimestamp(psutil.boot_time())
    uptime = datetime.now() - boot
    procs = check_processes()

    report = f"System Status — {datetime.now().strftime('%Y-%m-%d %H:%M')}\n"
    report += f"CPU: {cpu['percent']}% ({cpu['cores']} cores)\n"
    report += f"Memory: {mem['used_gb']}/{mem['total_gb']} GB ({mem['percent']}%)\n"
    report += f"Disk /: {disk['used_gb']}/{disk['total_gb']} GB ({disk['percent']}%)\n"
    report += f"Load: {load[0]:.1f} / {load[1]:.1f} / {load[2]:.1f}\n"
    report += f"Uptime: {uptime.days}d {uptime.seconds//3600}h\n"
    report += "\nTop Processes (CPU):\n"
    for p in procs:
        report += f"  {p['name']} (pid {p['pid']}) — {p['cpu']}% CPU, {p['mem_mb']} MB\n"
    return report


def check_alerts(thresholds=None):
    if thresholds is None:
        thresholds = {"cpu_percent": 90, "memory_percent": 85, "disk_percent": 90}
    alerts = []
    cpu = check_cpu()
    mem = check_memory()
    disk = check_disk()
    if cpu["percent"] > thresholds["cpu_percent"]:
        alerts.append(f"⚠️ CPU: {cpu['percent']}% > {thresholds['cpu_percent']}%")
    if mem["percent"] > thresholds["memory_percent"]:
        alerts.append(f"⚠️ Memory: {mem['percent']}% > {thresholds['memory_percent']}%")
    if disk["percent"] > thresholds["disk_percent"]:
        alerts.append(f"⚠️ Disk: {disk['percent']}% > {thresholds['disk_percent']}%")
    return alerts


def main():
    parser = argparse.ArgumentParser(description="System health check")
    parser.add_argument("cmd", nargs="?", default="status", choices=["status", "cpu", "memory", "disk", "processes", "alerts", "json"])
    args = parser.parse_args()

    if args.cmd == "status":
        print(full_report())
    elif args.cmd == "cpu":
        r = check_cpu()
        print(f"CPU: {r['percent']}% ({r['cores']} cores)")
    elif args.cmd == "memory":
        r = check_memory()
        print(f"Memory: {r['used_gb']}/{r['total_gb']} GB ({r['percent']}%)")
    elif args.cmd == "disk":
        r = check_disk()
        print(f"Disk: {r['used_gb']}/{r['total_gb']} GB ({r['percent']}%)")
    elif args.cmd == "processes":
        for p in check_processes(10):
            print(f"  {p['name']:20s} pid={p['pid']:6d}  CPU={p['cpu']:5.1f}%  MEM={p['mem_mb']:.0f}MB")
    elif args.cmd == "alerts":
        alerts = check_alerts()
        if alerts:
            for a in alerts:
                print(a)
        else:
            print("✓ All systems normal.")
    elif args.cmd == "json":
        print(json.dumps({"cpu": check_cpu(), "memory": check_memory(), "disk": check_disk()}, indent=2))


if __name__ == "__main__":
    main()
