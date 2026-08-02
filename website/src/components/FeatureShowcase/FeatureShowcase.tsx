import { CapabilitySection } from "./CapabilitySection";
import { DynamicRegistrationSection } from "./DynamicRegistrationSection";
import { DeviceDiscoverySection } from "./DeviceDiscoverySection";
import { ToolBoundarySection } from "./ToolBoundarySection";

export function FeatureShowcase() {
  return (
    <>
      <CapabilitySection />
      <DynamicRegistrationSection />
      <DeviceDiscoverySection />
      <ToolBoundarySection />
    </>
  );
}
