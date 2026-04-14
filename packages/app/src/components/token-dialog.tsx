import { useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useGithub } from "@/hooks/use-pull-data";

export function TokenDialog() {
  const { token, setToken } = useGithub();
  const [value, setValue] = useState(token ?? "");
  const [open, setOpen] = useState(false);

  function handleSave() {
    setToken(value || null);
    setOpen(false);
  }

  function handleClear() {
    setValue("");
    setToken(null);
    setOpen(false);
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="text-xs">
          {token ? "Token ✓" : "Connect GitHub"}
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>GitHub Token</DialogTitle>
          <DialogDescription>
            Enter a fine-grained GitHub personal access token to access private
            repos and increase rate limits.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <Input
            type="password"
            placeholder="github_pat_..."
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleSave();
            }}
          />
          <div className="flex gap-2">
            <Button onClick={handleSave} className="flex-1">
              Save
            </Button>
            {token && (
              <Button variant="outline" onClick={handleClear}>
                Clear
              </Button>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
