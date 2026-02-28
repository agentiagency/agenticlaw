#!/usr/bin/env python3
"""
Agent: Documentation Reader
Purpose: Read agent taxonomy and README files, report findings
"""

import sys
import json
from pathlib import Path

def read_file(filepath):
    """Read a file and return its contents"""
    try:
        with open(filepath, 'r') as f:
            return f.read()
    except Exception as e:
        return f"ERROR reading {filepath}: {str(e)}"

def main():
    if len(sys.argv) < 2:
        print(json.dumps({
            "status": "error",
            "message": "Usage: read_agent_docs.py <file1> [file2] ..."
        }))
        sys.exit(1)
    
    results = {}
    
    for filepath in sys.argv[1:]:
        path = Path(filepath).expanduser()
        results[str(path)] = {
            "exists": path.exists(),
            "size": path.stat().st_size if path.exists() else 0,
            "content": read_file(path) if path.exists() else "File not found"
        }
    
    print(json.dumps({
        "status": "success",
        "agent": "read_agent_docs",
        "files_read": len(results),
        "results": results
    }, indent=2))

if __name__ == "__main__":
    main()
