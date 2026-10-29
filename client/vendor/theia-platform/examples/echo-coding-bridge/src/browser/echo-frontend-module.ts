import { ContainerModule } from '@theia/core/shared/inversify';
import { FrontendApplicationContribution } from '@theia/core/lib/browser';
import { FileService } from '@theia/filesystem/lib/browser/file-service';
import { EchoFileService } from './echo-file-service';
import { EchoFrontendContribution } from './echo-frontend-contribution';
import { EchoEditorBridge } from './echo-editor-bridge';
import { ResourceResolver } from '@theia/core/lib/common/resource';
import { EchoDiffResources } from './echo-diff-resources';
import '../../src/browser/echo-workbench.css';

export default new ContainerModule((bind, _unbind, _isBound, rebind) => {
    rebind(FileService).to(EchoFileService).inSingletonScope();
    bind(EchoFrontendContribution).toSelf().inSingletonScope();
    bind(FrontendApplicationContribution).toService(EchoFrontendContribution);
    bind(EchoEditorBridge).toSelf().inSingletonScope();
    bind(FrontendApplicationContribution).toService(EchoEditorBridge);
    bind(EchoDiffResources).toSelf().inSingletonScope();
    bind(ResourceResolver).toService(EchoDiffResources);
});
