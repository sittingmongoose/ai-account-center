import { Link } from 'react-router-dom';
import { Home, Users } from 'lucide-react';
import {
  Sidebar,
  SidebarContent,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarHeader,
} from '@/components/ui/sidebar';
import { CcsLogo } from '@/components/shared/ccs-logo';

/** Kept for embedded clients; the dashboard itself uses a compact header. */
export function AppSidebar() {
  const links = [
    { href: '/', label: 'Accounts', icon: Home },
    { href: '/#codex', label: 'Codex accounts', icon: Users },
    { href: '/#claude', label: 'Claude accounts', icon: Users },
    { href: '/#usage', label: 'Other accounts', icon: Users },
  ];
  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <CcsLogo size="sm" />
      </SidebarHeader>
      <SidebarContent>
        <SidebarMenu>
          {links.map(({ href, label, icon: Icon }) => (
            <SidebarMenuItem key={href}>
              <SidebarMenuButton asChild tooltip={label}>
                <Link to={href}>
                  <Icon className="h-4 w-4" />
                  <span>{label}</span>
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
      </SidebarContent>
    </Sidebar>
  );
}
