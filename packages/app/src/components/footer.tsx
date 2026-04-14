import { Icon } from "@/components/brand";

export function Footer() {
  return (
    <footer className="flex w-full flex-col gap-3 py-4 text-xs leading-4 sm:flex-row sm:items-center">
      <div className="flex items-center gap-3 sm:flex-1">
        <Icon />
        <span>© 2026 Orlando Hohmeier</span>
      </div>
      <nav className="flex flex-wrap gap-x-3 gap-y-1">
        <a href="#" className="hover:underline">
          Terms
        </a>
        <a href="#" className="hover:underline">
          Privacy Policy
        </a>
        <a href="#" className="hover:underline">
          Cookie Policy
        </a>
        <a href="#" className="hover:underline">
          Imprint
        </a>
      </nav>
    </footer>
  );
}
